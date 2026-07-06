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
    interp_path, is_static_pie, load_dynamic_elf, load_span, load_static_elf, load_static_pie_elf,
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
// mmap arena (musl allocates via mmap) below the stack. The heap base is placed
// above the actual loaded image (not a fixed guess), and the stack is its own region
// with an unmapped guard band so overflow faults instead of corrupting the arena (#14).
const FLAT_SIZE: u64 = 0x800_0000; // 128 MiB (libc.so.6 ~2.4 MiB + arenas)
const EXE_BASE: u64 = 0x40_0000; // load bias for a PIE / static-PIE exe
const INTERP_BASE: u64 = 0x80_0000; // ld-linux/ld-musl bias (below the heap)
const HEAP_BASE_MIN: u64 = 0x100_0000; // heap starts here, or above the image if larger
const HEAP_SIZE: u64 = 0x80_0000; // 8 MiB brk arena between the heap base and mmap arena
const STACK_TOP: u64 = 0x7f0_0000;
const STACK_SIZE: u64 = 0x80_0000; // 8 MiB — matches the RLIMIT_STACK the shim reports
const STACK_GUARD: u64 = 0x10_0000; // unmapped guard below the stack: overflow faults
const STACK_BOTTOM: u64 = STACK_TOP - STACK_SIZE;
const MMAP_LIMIT: u64 = STACK_BOTTOM - STACK_GUARD; // mmap arena tops out below the guard
const PAGE: u64 = 0x1000;

// Go-runtime layout (go-caddy P1b). A Go program reserves a huge sparse virtual space
// at startup (mallocinit's ~600 MiB page-summary + a 768 GiB arena hint), which a
// 128 MiB Flat space can't back. When the entrypoint carries a Go build note, back it
// with a 1 TiB `Reserved` NORESERVE span instead and place the mmap arena high (where
// Go grows its heap), with the stack and brk kept low. All regions are sparse, so a
// 512 GiB arena costs no host memory until touched.
const GO_SPAN: u64 = 1 << 40; // 1 TiB — covers the 768 GiB arena hint
const GO_STACK_TOP: u64 = 0x8000_0000; // 2 GiB
const GO_STACK_BOTTOM: u64 = GO_STACK_TOP - STACK_SIZE;
const GO_MMAP_BASE: u64 = 0x1_0000_0000; // 4 GiB — mmap arena floor, clear of stack/brk
const GO_MMAP_LIMIT: u64 = GO_MMAP_BASE + (512 << 30); // 512 GiB arena
/// Cold blocks interpret, hot blocks JIT — one-shot image startup stays cheap.
const TIER_UP_AFTER: u32 = 50;

/// The per-image address-space layout `load_process` computes and the caller wires
/// into the shim (brk + mmap arena). Distinct per process because the heap base
/// depends on where the image's segments end.
struct Layout {
    brk: u64,
    brk_limit: u64,
    mmap_base: u64,
    mmap_limit: u64,
}

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
    run_config_argv_stdin(cfg, rootfs, engine, argv, &[])
}

/// Like [`run_config_argv`] but seeds the root process's stdin (fd 0) with `stdin`
/// — e.g. an HTTP request fed to `busybox httpd -i` (inetd mode), which serves a
/// file from the rootfs to stdout.
pub fn run_config_argv_stdin(
    cfg: &ImageConfig,
    rootfs: &Path,
    engine: EngineKind,
    argv: &[String],
    stdin: &[u8],
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
    let (vm, entry, rsp, layout, is_go) =
        load_process(rootfs, engine, &prog, &argv_bytes, &env_bytes)?;
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);
    let mut shim = LinuxShim::new();
    shim.serve_rootfs(rootfs);
    shim.stdin = stdin.to_vec();
    shim.brk = layout.brk;
    shim.brk_limit = layout.brk_limit;
    shim.mmap_base = layout.mmap_base;
    shim.mmap_limit = layout.mmap_limit;

    // A Go entrypoint is a threaded process: run it on the threaded driver directly.
    // It's structurally one-directional (P2.8) — a threaded process can't fork/execve —
    // so it never touches the deferred scheduler (and its Reserved span never hits the
    // fork-on-host-RAM panic). A shell tree that execs a Go binary is the escalation
    // case (task-126), which the direct-exec caddy image doesn't need.
    if is_go {
        let outcome = x86jit_linux::thread::run_threaded(vm, cpu, shim).map_err(|e| match e {
            ProcError::Trapped(m) => RunError::Trapped(m),
            ProcError::Exec(m) => RunError::Load(m),
        })?;
        return Ok(RunResult {
            stdout: outcome.stdout,
            exit_code: Some(outcome.exit_code),
        });
    }

    // The scheduler owns the process model; it calls back for ELF loading (which
    // lives here, not in the guest-agnostic embedder core) on every `execve`.
    let rootfs_buf = rootfs.to_path_buf();
    let exec_loader = move |req: &ExecRequest| -> Result<ExecImage, String> {
        // An exec'd Go binary would need escalation (task-126); the deferred scheduler
        // only ever runs non-Go processes, so the Flat layout from load_process is right.
        let (vm, entry, rsp, layout, _is_go) =
            load_process(&rootfs_buf, engine, &req.path, &req.argv, &req.envp)
                .map_err(|e| e.to_string())?;
        Ok(ExecImage {
            vm,
            entry,
            rsp,
            brk: layout.brk,
            brk_limit: layout.brk_limit,
            mmap_base: layout.mmap_base,
            mmap_limit: layout.mmap_limit,
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
) -> Result<(Vm, u64, u64, Layout, bool), RunError> {
    let prog_str = String::from_utf8_lossy(prog);
    // Resolve the entrypoint inside the rootfs, symlink-safe (the guest ELF's paths
    // are untrusted image metadata; a raw join would let `..`/symlinks escape).
    let host_path = resolve_in_rootfs(rootfs, prog)
        .ok_or_else(|| RunError::NoEntrypoint(prog_str.clone().into_owned()))?;
    let image =
        std::fs::read(&host_path).map_err(|_| RunError::NoEntrypoint(prog_str.into_owned()))?;

    // A Go entrypoint needs the huge Reserved span + the threaded driver (go-caddy P1b);
    // everything else stays on the default Flat space + deferred scheduler. Reserved is
    // opt-in precisely because a Flat guest that forks would panic on host-backed RAM and
    // Reserved widens the decision-3 divergence — so key it off the Go build note.
    let is_go = x86jit_elf::has_go_build_note(&image);
    let (stack_top, stack_bottom) = if is_go {
        (GO_STACK_TOP, GO_STACK_BOTTOM)
    } else {
        (STACK_TOP, STACK_BOTTOM)
    };

    let mut vm = if is_go {
        Vm::with_backend_host_ram(
            VmConfig {
                memory_model: MemoryModel::Reserved { span: GO_SPAN },
                consistency: MemConsistency::Fast,
            },
            engine.backend(),
            x86jit_linux::hostmem::reserve(GO_SPAN),
        )
    } else {
        Vm::with_backend(
            VmConfig {
                memory_model: MemoryModel::Flat { size: FLAT_SIZE },
                consistency: MemConsistency::Fast,
            },
            engine.backend(),
        )
    };
    if engine == EngineKind::Jit {
        vm.set_tier_up_after(Some(TIER_UP_AFTER));
        // Background tier-up (bg-tier, doc-27): compile hot blocks off the vcpu. Opt-in
        // via `X86JIT_BG_TIER` pending the flip decision (doc-27 #4); the bench shows it
        // 2.6-3.8x faster than inline tier-up on startup-heavy images, with no stall.
        if std::env::var_os("X86JIT_BG_TIER").is_some() {
            vm.set_tier_up_background(true);
        }
    }
    // Stack region up front (the loaders' `setup_stack` writes into it) — its own
    // mapping, leaving an unmapped guard band below it (#14).
    vm.map(stack_bottom, STACK_SIZE as usize, Prot::RW, RegionKind::Ram)
        .map_err(|e| RunError::Load(format!("map stack: {e:?}")))?;

    let argv_refs: Vec<&[u8]> = argv_bytes.iter().map(|v| v.as_slice()).collect();
    let env_refs: Vec<&[u8]> = env_bytes.iter().map(|v| v.as_slice()).collect();

    // Three load shapes: dynamic PIE (ld-linux/ld-musl from the rootfs), static-PIE
    // (ET_DYN, self-relocating static-musl), and ET_EXEC.
    let (entry, rsp) = if let Some(interp) = interp_path(&image) {
        let interp_host = resolve_in_rootfs(rootfs, interp.as_bytes())
            .ok_or_else(|| RunError::Load(format!("interpreter {interp} escapes rootfs")))?;
        let interp_bytes = std::fs::read(&interp_host)
            .map_err(|_| RunError::Load(format!("interpreter {interp} not found in rootfs")))?;
        // Place the interpreter clear of the executable's own span. A big PIE (e.g.
        // ubuntu's ~11 MiB uutils `coreutils`) loaded at EXE_BASE overruns a fixed
        // low INTERP_BASE, colliding the two mappings; derive the base above the
        // exe's mapped end (1 MiB-aligned, 1 MiB gap), never below the floor.
        let interp_base = match load_span(&image) {
            Some((_, hi)) => {
                let exe_end = (EXE_BASE + hi + PAGE - 1) & !(PAGE - 1);
                ((exe_end + 0x10_0000) & !(0x10_0000 - 1)).max(INTERP_BASE)
            }
            None => INTERP_BASE,
        };
        let img = load_dynamic_elf(&mut vm, &image, EXE_BASE, &interp_bytes, interp_base)
            .map_err(|e| RunError::Load(format!("dynamic: {e:?}")))?;
        let rsp = setup_stack_dyn(&mut vm, stack_top, &argv_refs, &env_refs, &img)
            .map_err(|e| RunError::Load(format!("stack: {e:?}")))?;
        (img.entry, rsp)
    } else if is_static_pie(&image) {
        let img = load_static_pie_elf(&mut vm, &image, EXE_BASE)
            .map_err(|e| RunError::Load(format!("static-pie: {e:?}")))?;
        let rsp = setup_stack_dyn(&mut vm, stack_top, &argv_refs, &env_refs, &img)
            .map_err(|e| RunError::Load(format!("stack: {e:?}")))?;
        (img.entry, rsp)
    } else {
        let entry =
            load_static_elf(&mut vm, &image).map_err(|e| RunError::Load(format!("{e:?}")))?;
        let rsp = setup_stack(&mut vm, stack_top, &argv_refs, &env_refs)
            .map_err(|e| RunError::Load(format!("stack: {e:?}")))?;
        (entry, rsp)
    };

    let layout = if is_go {
        // Go layout: a small brk arena just above the image, and the mmap arena high (at
        // GO_MMAP_BASE), where Go grows its heap. Both regions are sparse over the
        // Reserved span, so the 512 GiB arena is free until touched. brk and mmap are
        // separate regions (unlike Flat's single span) because the low stack sits between.
        let image_top = vm
            .mem
            .highest_mapped_below(GO_STACK_BOTTOM)
            .max(HEAP_BASE_MIN);
        let brk = (image_top + PAGE - 1) & !(PAGE - 1);
        let brk_limit = brk + HEAP_SIZE;
        vm.map(brk, (brk_limit - brk) as usize, Prot::RW, RegionKind::Ram)
            .map_err(|e| RunError::Load(format!("map go brk: {e:?}")))?;
        vm.map(
            GO_MMAP_BASE,
            (GO_MMAP_LIMIT - GO_MMAP_BASE) as usize,
            Prot::RW,
            RegionKind::Ram,
        )
        .map_err(|e| RunError::Load(format!("map go mmap arena: {e:?}")))?;
        Layout {
            brk,
            brk_limit,
            mmap_base: GO_MMAP_BASE,
            mmap_limit: GO_MMAP_LIMIT,
        }
    } else {
        // Place the heap just above the image's loaded segments (not a fixed guess), then
        // the mmap arena above that, capped below the stack guard. A image whose segments
        // reach that cap is rejected with a clear error instead of silently colliding (#14).
        let image_top = vm.mem.highest_mapped_below(STACK_BOTTOM).max(HEAP_BASE_MIN);
        let brk = (image_top + PAGE - 1) & !(PAGE - 1);
        let mmap_base = brk + HEAP_SIZE;
        if mmap_base >= MMAP_LIMIT {
            return Err(RunError::Load(format!(
                "image segments end at {image_top:#x}; no room for heap+mmap below the stack \
                 guard at {MMAP_LIMIT:#x}"
            )));
        }
        vm.map(brk, (MMAP_LIMIT - brk) as usize, Prot::RW, RegionKind::Ram)
            .map_err(|e| RunError::Load(format!("map heap: {e:?}")))?;
        Layout {
            brk,
            brk_limit: mmap_base,
            mmap_base,
            mmap_limit: MMAP_LIMIT,
        }
    };
    Ok((vm, entry, rsp, layout, is_go))
}

// Compile-time layout invariants (#14). The stack budget must match the
// RLIMIT_STACK the shim reports (8 MiB) and sit above an unmapped guard band, so a
// guest trusting `getrlimit` can't silently grow its stack into the mmap arena.
const _: () = {
    assert!(STACK_SIZE == 8 * 1024 * 1024); // matches the 8 MiB rlimit the shim reports
    assert!(STACK_GUARD > 0); // an unmapped guard sits below the stack
    assert!(MMAP_LIMIT < STACK_BOTTOM); // guard band separates the mmap arena from the stack
    assert!(HEAP_BASE_MIN + HEAP_SIZE < MMAP_LIMIT); // room for heap + mmap below the guard
    assert!(STACK_TOP <= FLAT_SIZE); // stack fits in the flat model
};

// Compile-time invariants for the Go/Reserved layout (#14). Unlike Flat, the mmap arena
// sits *above* the stack, and everything lives inside the Reserved span. brk (just above
// the image, ≥ HEAP_BASE_MIN) + its 8 MiB arena must stay clear below the stack.
const _: () = {
    assert!(GO_STACK_BOTTOM > HEAP_BASE_MIN + HEAP_SIZE); // brk arena fits below the stack
    assert!(GO_MMAP_BASE > GO_STACK_TOP); // arena is clear above the stack region
    assert!(GO_MMAP_LIMIT <= GO_SPAN); // the whole layout fits inside the Reserved span
};
