//! Run a program on the x86jit recompiler — the library behind the `x86jit-cli`
//! binary.
//!
//! Glue only: the [`oci`] module turns a `docker save` tar into a rootfs + config,
//! [`x86jit_elf`] loads the entrypoint, [`x86jit_linux`] services syscalls, and the
//! engine executes it. The `run_*` family drives a run at increasing levels of
//! control; the binary's `run` subcommand runs a host ELF, its `oci` subcommand a
//! `docker save` image.

/// `docker save` image reader (rootfs + config). Kept as a self-contained module
/// with **no dependency on `x86jit_core`** — reading an image has nothing to do with
/// the recompiler (spec §1/§4.1), so nothing here may `use x86jit_core`.
pub mod oci;

/// OCI/Docker **registry** client — pull an image by reference into a rootfs. Also
/// core-free (it only fetches bytes over HTTP and reuses `oci`'s layer/config logic).
pub mod registry;

use std::path::Path;

pub use oci::{load_image, ImageConfig, OciError};

use x86jit_core::{Backend, InterpreterBackend, Prot, Reg, RegionCaps, RegionKind, Vm, VmConfig};
// Re-export so embedders (and x86jit-cli) select the guest ISA level without a direct
// x86jit-core dependency (task-169).
pub use x86jit_core::GuestCpuFeatures;
// Re-export the JIT's host-codegen knob so an embedder can pin it via `EngineConfig`.
pub use x86jit_cranelift::HostTarget;
use x86jit_cranelift::JitBackend;

/// Superblock caps for the region-forming JIT mode (mirrors the superblock tests).
const BG_REGION_CAPS: RegionCaps = RegionCaps {
    max_blocks: 16,
    max_icount: 256,
};
use x86jit_elf::{
    interp_path, is_static_pie, load_dynamic_elf, load_span, load_static_elf, load_static_pie_elf,
    setup_stack, setup_stack_dyn,
};
use x86jit_linux::shim::{resolve_in_rootfs, ExecRequest};
use x86jit_linux::{ExecImage, LinuxShim, ProcError, Scheduler};

/// Which engine to run under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Interpreter,
    Jit,
}

/// When a block gets JIT-compiled. `Off` is the interpreter (never tiers up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TierUp {
    /// Stay interpreted — no compilation (the interpreter engine).
    Off,
    /// Compile hot blocks inline on the vcpu once past the threshold.
    Inline,
    /// Compile hot blocks on a background thread the JIT owns, off the vcpu path.
    Background,
}

/// How to run: the engine plus its JIT tuning. Construct one explicitly for full
/// control, or use [`EngineConfig::from_env`] / `EngineKind::into()` to fold in the
/// `X86JIT_*` environment overrides (the default the binary and tests use).
///
/// This is where the JIT tuning knobs live now, instead of scattered `std::env`
/// reads inside the library: env parsing happens once at the edge ([`from_env`]),
/// and everything downstream takes an explicit `EngineConfig` (task-181).
///
/// [`from_env`]: EngineConfig::from_env
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    pub kind: EngineKind,
    pub tier_up: TierUp,
    /// Form superblock regions for proven-hot loops (implies background tier-up).
    pub superblocks: bool,
    /// The host ISA Cranelift targets (`Native`, or `Baseline` for portable codegen).
    pub host_target: HostTarget,
}

impl Default for EngineConfig {
    /// The historical default: JIT, inline tier-up, no superblocks, native host.
    fn default() -> Self {
        EngineConfig {
            kind: EngineKind::Jit,
            tier_up: TierUp::Inline,
            superblocks: false,
            host_target: HostTarget::Native,
        }
    }
}

impl EngineConfig {
    /// Build a config for `kind`, folding in the `X86JIT_*` overrides — the escape
    /// hatch the binary and tests rely on. The interpreter never tiers up.
    pub fn from_env(kind: EngineKind) -> Self {
        if kind == EngineKind::Interpreter {
            return EngineConfig {
                kind,
                tier_up: TierUp::Off,
                superblocks: false,
                host_target: HostTarget::Native,
            };
        }
        // BGT-6 (doc-27 Phase 6): `X86JIT_BG_REGION` forms superblock regions for hot
        // loops and compiles them in the background (implies bg-tier). `X86JIT_BG_TIER`
        // (doc-27 #4) moves tier-up off the vcpu without regions. Both off by default.
        let superblocks = std::env::var_os("X86JIT_BG_REGION").is_some();
        let background = superblocks || std::env::var_os("X86JIT_BG_TIER").is_some();
        // `X86JIT_HOST_BASELINE=1` pins Cranelift below the host — no AVX, for
        // deterministic/portable codegen (task-175). Off by default (native host).
        let host_target = if std::env::var_os("X86JIT_HOST_BASELINE").is_some() {
            HostTarget::Baseline
        } else {
            HostTarget::Native
        };
        EngineConfig {
            kind,
            tier_up: if background {
                TierUp::Background
            } else {
                TierUp::Inline
            },
            superblocks,
            host_target,
        }
    }

    fn backend(&self) -> Box<dyn Backend> {
        match self.kind {
            EngineKind::Interpreter => Box::new(InterpreterBackend),
            // Superblocks take precedence over a pinned host target (as when the knobs
            // were separate env branches): region formation needs its own ctor.
            EngineKind::Jit if self.superblocks => {
                Box::new(JitBackend::with_superblocks(BG_REGION_CAPS))
            }
            EngineKind::Jit if self.host_target == HostTarget::Baseline => {
                Box::new(JitBackend::with_host_target(HostTarget::Baseline))
            }
            EngineKind::Jit => Box::new(JitBackend::new()),
        }
    }
}

impl From<EngineKind> for EngineConfig {
    /// A bare `EngineKind` folds in the `X86JIT_*` env overrides, so existing callers
    /// (the binary, tests) keep today's behavior; pass an `EngineConfig` to bypass env.
    fn from(kind: EngineKind) -> Self {
        EngineConfig::from_env(kind)
    }
}

/// Observable result of a run: captured stdout + stderr + guest exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub stdout: Vec<u8>,
    /// Captured stderr (task-129) — a guest's fd-2 diagnostics (a Go panic, caddy's
    /// boot errors) instead of being dropped.
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
}

#[derive(Debug)]
pub enum RunError {
    Oci(OciError),
    /// Pulling the image from a registry failed (`oci run <ref>`).
    Registry(registry::RegistryError),
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
            RunError::Registry(e) => write!(f, "{e}"),
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
impl From<registry::RegistryError> for RunError {
    fn from(e: registry::RegistryError) -> Self {
        RunError::Registry(e)
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
    engine: impl Into<EngineConfig>,
) -> Result<RunResult, RunError> {
    let cfg = load_image(image_tar, rootfs)?;
    run_config(&cfg, rootfs, engine)
}

/// Pull `reference` from a registry into `rootfs` (must exist) and run it under
/// `engine` — the programmatic `x86jit-cli oci run <ref>`. An empty `argv` uses the
/// image's default `Entrypoint`+`Cmd`; otherwise it overrides them. `plain_http`
/// selects an insecure `http://` registry (e.g. a local `registry:5000`).
pub fn run_registry(
    reference: &str,
    rootfs: &Path,
    engine: impl Into<EngineConfig>,
    argv: &[String],
    opts: RunOptions,
    plain_http: bool,
) -> Result<RunResult, RunError> {
    let cfg = registry::pull(reference, rootfs, plain_http)?;
    let argv = if argv.is_empty() {
        cfg.argv()
    } else {
        argv.to_vec()
    };
    run_config_argv_opts(&cfg, rootfs, engine, &argv, opts)
}

/// Run a pre-extracted rootfs + config (so a caller can extract once and run both
/// engines), using the image's default `Entrypoint`+`Cmd`.
pub fn run_config(
    cfg: &ImageConfig,
    rootfs: &Path,
    engine: impl Into<EngineConfig>,
) -> Result<RunResult, RunError> {
    run_config_argv(cfg, rootfs, engine, &cfg.argv())
}

/// Run with an explicit `argv` override (e.g. a specific busybox applet instead of
/// the image's default `sh`). `argv[0]` is resolved as the entrypoint path.
pub fn run_config_argv(
    cfg: &ImageConfig,
    rootfs: &Path,
    engine: impl Into<EngineConfig>,
    argv: &[String],
) -> Result<RunResult, RunError> {
    run_config_argv_opts(cfg, rootfs, engine, argv, RunOptions::default())
}

/// Per-run options (task-171): stdin seed + guest ISA level. Add future per-run knobs
/// here instead of growing another `run_config_argv_*` wrapper.
#[derive(Clone, Default)]
pub struct RunOptions {
    /// Bytes fed to the root process's stdin (fd 0) — e.g. an HTTP request to
    /// `busybox httpd -i`. Empty by default.
    pub stdin: Vec<u8>,
    /// Guest CPU feature set / ISA level (task-169). Default is the built-in set;
    /// e.g. `GuestCpuFeatures::v4()` to run an x86-64-v4 binary.
    pub features: GuestCpuFeatures,
}

/// Thin shim over [`run_config_argv_opts`] (task-171): stdin seed, default features.
pub fn run_config_argv_stdin(
    cfg: &ImageConfig,
    rootfs: &Path,
    engine: impl Into<EngineConfig>,
    argv: &[String],
    stdin: &[u8],
) -> Result<RunResult, RunError> {
    let opts = RunOptions {
        stdin: stdin.to_vec(),
        ..Default::default()
    };
    run_config_argv_opts(cfg, rootfs, engine, argv, opts)
}

/// Thin shim over [`run_config_argv_opts`] (task-169/171): stdin seed + explicit ISA level.
pub fn run_config_argv_stdin_features(
    cfg: &ImageConfig,
    rootfs: &Path,
    engine: impl Into<EngineConfig>,
    argv: &[String],
    stdin: &[u8],
    features: GuestCpuFeatures,
) -> Result<RunResult, RunError> {
    let opts = RunOptions {
        stdin: stdin.to_vec(),
        features,
    };
    run_config_argv_opts(cfg, rootfs, engine, argv, opts)
}

/// Canonical run entry (task-171): run `argv` under `engine` with `opts` (stdin + guest
/// ISA level). `argv[0]` is the entrypoint path. Advertising a feature past the lifter's
/// coverage surfaces as a guest trap.
pub fn run_config_argv_opts(
    cfg: &ImageConfig,
    rootfs: &Path,
    engine: impl Into<EngineConfig>,
    argv: &[String],
    opts: RunOptions,
) -> Result<RunResult, RunError> {
    // Resolve the engine once here (reading `X86JIT_*` if a bare `EngineKind` was
    // passed); everything downstream takes the concrete, env-free `EngineConfig`.
    let engine: EngineConfig = engine.into();
    let features = opts.features;
    let stdin: &[u8] = &opts.stdin;
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
    let (mut vm, entry, rsp, layout, is_go) =
        load_process(rootfs, engine, &prog, &argv_bytes, &env_bytes)?;
    vm.set_guest_cpu_features(features); // guest ISA level (task-169)
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
            stderr: outcome.stderr,
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
        stderr: outcome.stderr,
        exit_code: Some(outcome.exit_code),
    })
}

/// Build a fresh `Vm`, load `prog` (resolved in the rootfs) with the right ELF
/// shape, and set up its initial stack. Returns the vm + entry + rsp.
fn load_process(
    rootfs: &Path,
    engine: EngineConfig,
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
    // everything else stays on the Flat space + deferred scheduler. Both spans are now
    // host-backed and guarded (GP-5); they differ in size (Flat 128 MiB copyable on fork,
    // Reserved 1 TiB not) and driver (threaded vs scheduler) — so key it off the Go note.
    let is_go = x86jit_elf::has_go_build_note(&image);
    let (stack_top, stack_bottom) = if is_go {
        (GO_STACK_TOP, GO_STACK_BOTTOM)
    } else {
        (STACK_TOP, STACK_BOTTOM)
    };

    let mut vm = if is_go {
        Vm::with_backend_host_ram(
            VmConfig::reserved(GO_SPAN),
            engine.backend(),
            // Guarded: an in-span-unmapped access (a Go nil-deref) hardware-faults and
            // is recovered to Exit::UnmappedMemory under the JIT (doc-30, task-127).
            x86jit_linux::hostmem::reserve_guarded(GO_SPAN),
        )
    } else {
        Vm::with_backend_host_ram(
            VmConfig::flat(FLAT_SIZE),
            engine.backend(),
            // Guarded (doc-30 GP-5): the Flat span's unmapped holes — the nil page,
            // the #14 stack guard band, inter-mapping gaps — are PROT_NONE, so a wild
            // in-span pointer hardware-faults into Exit::UnmappedMemory under the JIT,
            // matching the interpreter. A forking Flat guest deep-copies to a
            // Vec-backed child (Memory::deep_copy skips the guards); execve reloads a
            // fresh guarded span.
            x86jit_linux::hostmem::reserve_guarded(FLAT_SIZE),
        )
    };
    // Tier-up policy from the resolved config (task-181). Inline: compile hot blocks
    // on the vcpu; Background (bg-tier, doc-27): compile off the vcpu on the JIT's own
    // thread — the bench shows it 2.6-3.8x faster than inline on startup-heavy images.
    match engine.tier_up {
        TierUp::Off => {}
        TierUp::Inline => vm.set_tier_up_after(Some(TIER_UP_AFTER)),
        TierUp::Background => {
            vm.set_tier_up_after(Some(TIER_UP_AFTER));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_the_historical_jit() {
        let d = EngineConfig::default();
        assert_eq!(d.kind, EngineKind::Jit);
        assert_eq!(d.tier_up, TierUp::Inline);
        assert!(!d.superblocks);
        assert_eq!(d.host_target, HostTarget::Native);
    }

    #[test]
    fn interpreter_never_tiers_and_ignores_jit_env() {
        // Independent of any X86JIT_* var: the interpreter has no JIT to tune.
        let c = EngineConfig::from_env(EngineKind::Interpreter);
        assert_eq!(c.kind, EngineKind::Interpreter);
        assert_eq!(c.tier_up, TierUp::Off);
        assert!(!c.superblocks);
    }

    #[test]
    fn engine_kind_converts_via_from_env() {
        // A bare EngineKind folds in the env overrides (identity of kind preserved).
        let c: EngineConfig = EngineKind::Interpreter.into();
        assert_eq!(c, EngineConfig::from_env(EngineKind::Interpreter));
    }
}
