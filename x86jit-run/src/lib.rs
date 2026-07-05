//! Run an OCI/Docker image on the x86jit recompiler (OCI-1.T4).
//!
//! Glue only: [`x86jit_oci`] turns a `docker save` tar into a rootfs + config,
//! [`x86jit_elf`] loads the entrypoint, [`x86jit_linux`] services syscalls, and the
//! engine executes it. The MVP runs a single static entrypoint (no fork, no
//! rootfs file access) — enough for `hello-world`; the guest filesystem and the
//! process model land in later OCI rungs.

use std::path::Path;

use x86jit_core::{
    Backend, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{
    interp_path, is_static_pie, load_dynamic_elf, load_static_elf, load_static_pie_elf,
    setup_stack, setup_stack_dyn,
};
use x86jit_linux::shim::{resolve_in_rootfs, ExecRequest};
use x86jit_linux::{ExecImage, LinuxShim, ProcError, Scheduler};
use x86jit_oci::{load_image, ImageConfig, OciError};

/// Which engine to run under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Interpreter,
    Jit,
}

impl EngineKind {
    fn backend(self) -> Box<dyn Backend> {
        match self {
            EngineKind::Interpreter => Box::new(InterpreterBackend),
            EngineKind::Jit => Box::new(JitBackend::new()),
        }
    }
}

/// Observable result of a run: captured stdout + guest exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub stdout: Vec<u8>,
    pub exit_code: Option<i32>,
}

#[derive(Debug)]
pub enum RunError {
    Oci(OciError),
    /// The entrypoint path from the image config wasn't found in the rootfs.
    NoEntrypoint(String),
    /// ELF load / execution problem.
    Load(String),
    /// The guest trapped on something the MVP runner doesn't handle yet (an
    /// unknown instruction, an unhandled syscall exit, MMIO, …).
    Trapped(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Oci(e) => write!(f, "{e}"),
            RunError::NoEntrypoint(p) => write!(f, "entrypoint {p} not found in rootfs"),
            RunError::Load(m) => write!(f, "load: {m}"),
            RunError::Trapped(m) => write!(f, "guest trapped: {m}"),
        }
    }
}
impl std::error::Error for RunError {}
impl From<OciError> for RunError {
    fn from(e: OciError) -> Self {
        RunError::Oci(e)
    }
}

// Guest memory layout. Generous flat model covering an ET_EXEC image (at its own
// ~0x400000 vaddrs) or a static-PIE image (loaded at PIE_BASE), plus a heap and an
// mmap arena (musl allocates via mmap) below the stack.
const FLAT_SIZE: u64 = 0x800_0000; // 128 MiB (libc.so.6 ~2.4 MiB + arenas)
const EXE_BASE: u64 = 0x40_0000; // load bias for a PIE / static-PIE exe
const INTERP_BASE: u64 = 0x80_0000; // ld-linux/ld-musl bias (below the heap)
const HEAP_BASE: u64 = 0x100_0000;
const MMAP_BASE: u64 = 0x180_0000;
const STACK_TOP: u64 = 0x7f0_0000;
/// Cold blocks interpret, hot blocks JIT — one-shot image startup stays cheap.
const TIER_UP_AFTER: u32 = 50;

/// Extract `image_tar` into `rootfs` (must exist) and run its entrypoint under
/// `engine`. Returns captured stdout + exit code.
pub fn run_image(
    image_tar: &Path,
    rootfs: &Path,
    engine: EngineKind,
) -> Result<RunResult, RunError> {
    let cfg = load_image(image_tar, rootfs)?;
    run_config(&cfg, rootfs, engine)
}

/// Run a pre-extracted rootfs + config (so a caller can extract once and run both
/// engines), using the image's default `Entrypoint`+`Cmd`.
pub fn run_config(
    cfg: &ImageConfig,
    rootfs: &Path,
    engine: EngineKind,
) -> Result<RunResult, RunError> {
    run_config_argv(cfg, rootfs, engine, &cfg.argv())
}

/// Run with an explicit `argv` override (e.g. a specific busybox applet instead of
/// the image's default `sh`). `argv[0]` is resolved as the entrypoint path.
pub fn run_config_argv(
    cfg: &ImageConfig,
    rootfs: &Path,
    engine: EngineKind,
    argv: &[String],
) -> Result<RunResult, RunError> {
    let prog: Vec<u8> = argv
        .first()
        .ok_or_else(|| RunError::NoEntrypoint("<empty Cmd/Entrypoint>".into()))?
        .clone()
        .into_bytes();
    let argv_bytes: Vec<Vec<u8>> = argv.iter().map(|s| s.as_bytes().to_vec()).collect();
    let env_bytes: Vec<Vec<u8>> = cfg.env.iter().map(|s| s.as_bytes().to_vec()).collect();

    // The root process: load its image and give it a rootfs-serving shim. The shim's
    // fds and stdout persist across the whole tree (execve keeps them; fork shares
    // them) — a guest `execve` reloads the image, `fork`/`wait4` build the tree.
    let (vm, entry, rsp) = load_process(rootfs, engine, &prog, &argv_bytes, &env_bytes)?;
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);
    let mut shim = LinuxShim::new();
    shim.serve_rootfs(rootfs);
    shim.brk = HEAP_BASE;
    shim.brk_limit = MMAP_BASE;
    shim.mmap_base = MMAP_BASE;
    shim.mmap_limit = STACK_TOP - 0x10_0000;

    // The scheduler owns the process model; it calls back for ELF loading (which
    // lives here, not in the guest-agnostic embedder core) on every `execve`.
    let rootfs_buf = rootfs.to_path_buf();
    let exec_loader = move |req: &ExecRequest| -> Result<ExecImage, String> {
        let (vm, entry, rsp) =
            load_process(&rootfs_buf, engine, &req.path, &req.argv, &req.envp)
                .map_err(|e| e.to_string())?;
        Ok(ExecImage {
            vm,
            entry,
            rsp,
            brk: HEAP_BASE,
            brk_limit: MMAP_BASE,
            mmap_base: MMAP_BASE,
            mmap_limit: STACK_TOP - 0x10_0000,
        })
    };
    let mut sched = Scheduler::new(move || engine.backend()).with_exec_loader(exec_loader);
    let outcome = sched.run(vm, cpu, shim).map_err(|e| match e {
        ProcError::Trapped(m) => RunError::Trapped(m),
        ProcError::Exec(m) => RunError::Load(m),
    })?;
    Ok(RunResult {
        stdout: outcome.stdout,
        exit_code: Some(outcome.exit_code),
    })
}

/// Build a fresh `Vm`, load `prog` (resolved in the rootfs) with the right ELF
/// shape, and set up its initial stack. Returns the vm + entry + rsp.
fn load_process(
    rootfs: &Path,
    engine: EngineKind,
    prog: &[u8],
    argv_bytes: &[Vec<u8>],
    env_bytes: &[Vec<u8>],
) -> Result<(Vm, u64, u64), RunError> {
    let prog_str = String::from_utf8_lossy(prog);
    // Resolve the entrypoint inside the rootfs, symlink-safe (the guest ELF's paths
    // are untrusted image metadata; a raw join would let `..`/symlinks escape).
    let host_path = resolve_in_rootfs(rootfs, prog)
        .ok_or_else(|| RunError::NoEntrypoint(prog_str.clone().into_owned()))?;
    let image = std::fs::read(&host_path)
        .map_err(|_| RunError::NoEntrypoint(prog_str.into_owned()))?;

    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT_SIZE },
            consistency: MemConsistency::Fast,
        },
        engine.backend(),
    );
    if engine == EngineKind::Jit {
        vm.set_tier_up_after(Some(TIER_UP_AFTER));
    }
    vm.map(
        HEAP_BASE,
        (FLAT_SIZE - HEAP_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .map_err(|e| RunError::Load(format!("map ram: {e:?}")))?;

    let argv_refs: Vec<&[u8]> = argv_bytes.iter().map(|v| v.as_slice()).collect();
    let env_refs: Vec<&[u8]> = env_bytes.iter().map(|v| v.as_slice()).collect();

    // Three load shapes: dynamic PIE (ld-linux/ld-musl from the rootfs), static-PIE
    // (ET_DYN, self-relocating static-musl), and ET_EXEC.
    let (entry, rsp) = if let Some(interp) = interp_path(&image) {
        let interp_host = resolve_in_rootfs(rootfs, interp.as_bytes())
            .ok_or_else(|| RunError::Load(format!("interpreter {interp} escapes rootfs")))?;
        let interp_bytes = std::fs::read(&interp_host)
            .map_err(|_| RunError::Load(format!("interpreter {interp} not found in rootfs")))?;
        let img = load_dynamic_elf(&mut vm, &image, EXE_BASE, &interp_bytes, INTERP_BASE)
            .map_err(|e| RunError::Load(format!("dynamic: {e:?}")))?;
        let rsp = setup_stack_dyn(&mut vm, STACK_TOP, &argv_refs, &env_refs, &img)
            .map_err(|e| RunError::Load(format!("stack: {e:?}")))?;
        (img.entry, rsp)
    } else if is_static_pie(&image) {
        let img = load_static_pie_elf(&mut vm, &image, EXE_BASE)
            .map_err(|e| RunError::Load(format!("static-pie: {e:?}")))?;
        let rsp = setup_stack_dyn(&mut vm, STACK_TOP, &argv_refs, &env_refs, &img)
            .map_err(|e| RunError::Load(format!("stack: {e:?}")))?;
        (img.entry, rsp)
    } else {
        let entry =
            load_static_elf(&mut vm, &image).map_err(|e| RunError::Load(format!("{e:?}")))?;
        let rsp = setup_stack(&mut vm, STACK_TOP, &argv_refs, &env_refs)
            .map_err(|e| RunError::Load(format!("stack: {e:?}")))?;
        (entry, rsp)
    };
    Ok((vm, entry, rsp))
}
