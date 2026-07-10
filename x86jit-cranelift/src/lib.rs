//! Cranelift JIT backend for x86jit (§8.2).
//!
//! Compiles an [`x86jit_core::IrBlock`] to host code. Guest RAM access is inlined
//! (`host_base + guest_addr` after a bounds check); only out-of-range access and
//! syscalls trap out. The compiled-block ABI (signature, result encoding, field
//! offsets) is defined once in `x86jit_core::jit_abi` and shared with the
//! dispatcher; this crate only emits code matching it.
//!
//! Build order (§8.2.3): offsets + ABI + a "returns Continue" block first, then
//! `IrOp`s one at a time, each validated against the interpreter oracle.

#![cfg(feature = "jit")]

mod codegen;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use x86jit_core::cache::CompiledPtr;
use x86jit_core::jit_abi::{cpu_offsets, CpuOffsets};
use x86jit_core::{
    Backend, CachedBlock, IrBlock, IrRegion, MemConsistency, RegionCaps, TierUpFinished,
    TierUpRequest, TierUpSubmit, TierUpUnit,
};

/// Division helper called from compiled code (div isn't hot, so a call is fine and
/// avoids 128-bit codegen). Reuses the interpreter's `divide` so both agree.
/// `out` points at `[quot, rem]`; returns 0 on success, 1 on `#DE`.
///
/// # Safety
/// `out` must point at two writable `u64`s. Called only from JIT code with a valid
/// stack-slot pointer.
unsafe extern "C" fn div_helper(
    hi: u64,
    lo: u64,
    divisor: u64,
    size: u64,
    signed: u64,
    out: *mut u64,
) -> u64 {
    match x86jit_core::interp::divide(hi, lo, divisor, size as u8, signed != 0) {
        Some((q, r)) => {
            *out = q;
            *out.add(1) = r;
            0
        }
        None => 1,
    }
}

/// String-op helper: runs the whole (rep) loop via the shared `string_run`. Reads
/// `cpu` and the guest buffer (`MemCtx.base`/`size`); on a trap it writes the
/// fault into `MemCtx` and returns `RET_UNMAPPED`, else `RET_CONTINUE`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
unsafe extern "C" fn string_helper(
    cpu: *mut u8,
    mem: *mut u8,
    op: u64,
    elem: u64,
    rep: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    use x86jit_core::{RepKind, StrOp};

    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let op = [
        StrOp::Movs,
        StrOp::Stos,
        StrOp::Scas,
        StrOp::Cmps,
        StrOp::Lods,
    ][op as usize];
    let rep = [RepKind::None, RepKind::Rep, RepKind::Repe, RepKind::Repne][rep as usize];

    // Raw bounds-only view: the JIT's inlined stores skip SMC/region handling
    // (deferred, §10), so its string helper matches — no `Memory` in the ABI.
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::interp::string_run(cpu, &raw, op, elem as u8, rep, cur_addr) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// x87 helper: runs one FPU op via the shared `exec_x87`. On a memory fault it
/// writes the fault into `MemCtx`, sets RIP to the faulting instruction, and
/// returns `RET_UNMAPPED`; otherwise `RET_CONTINUE`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call; `kind`
/// is a valid `FpuKind` discriminant (the lift only emits real ones).
unsafe extern "C" fn x87_helper(
    cpu: *mut u8,
    mem: *mut u8,
    kind: u64,
    addr: u64,
    sti: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    // Safe: `kind` came from a real `FpuKind as u16` baked by the lift.
    let kind: x86jit_core::x87::FpuKind = std::mem::transmute(kind as u16);
    let raw = x86jit_core::x87::RawFpMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::x87::exec_x87(cpu, &raw, kind, addr, sti as u8) {
        None => RET_CONTINUE,
        Some((fault, write)) => {
            ctx.fault_addr = fault;
            ctx.fault_access = write as u64;
            cpu.rip = cur_addr;
            RET_UNMAPPED
        }
    }
}

/// fxsave/fxrstor helper: runs the 512-byte save/restore via the shared
/// `exec_fxstate`. On a memory fault it sets RIP + fault fields and returns
/// `RET_UNMAPPED`.
///
/// # Safety
/// `cpu`/`mem` are valid for the call; `mem` is a `*mut MemCtx`.
unsafe extern "C" fn fxstate_helper(
    cpu: *mut u8,
    mem: *mut u8,
    addr: u64,
    restore: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::x87::RawFpMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::x87::exec_fxstate(cpu, &raw, addr, restore != 0) {
        None => RET_CONTINUE,
        Some((fault, write)) => {
            ctx.fault_addr = fault;
            ctx.fault_access = write as u64;
            cpu.rip = cur_addr;
            RET_UNMAPPED
        }
    }
}

/// `cpuid` helper: delegates to the shared `cpuid_run` so both backends report the
/// same features.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn cpuid_helper(cpu: *mut u8) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::cpuid_run(cpu);
}

/// `xgetbv` helper: delegates to the shared `xgetbv_run` so XCR0 tracks the guest
/// feature set (task-169) identically on both backends.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn xgetbv_helper(cpu: *mut u8) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::xgetbv_run(cpu);
}

/// `crc32` helper: CRC-32C folding via the shared `crc32c` so both backends agree.
extern "C" fn crc32_helper(crc: u64, src: u64, bytes: u64) -> u64 {
    x86jit_core::interp::crc32c(crc as u32, src, bytes as u8) as u64
}

/// BMI1/BMI2 helper (task-168.5.3): runs the shared `bmi_result` so the JIT matches
/// the interpreter exactly (the bextr/bzhi variable shift+mask is fiddly to emit
/// natively). Writes `out[0] = result`, `out[1] = CF`; ZF/SF are derived at the call
/// site. `op` is the `BmiOp` discriminant.
///
/// # Safety
/// `out` points to two writable `u64`s for the call.
unsafe extern "C" fn bmi_helper(a: u64, b: u64, op: u64, size: u64, out: *mut u64) {
    use x86jit_core::ir::BmiOp::*;
    let bmiop = match op {
        0 => Andn,
        1 => Blsi,
        2 => Blsr,
        3 => Blsmsk,
        4 => Bextr,
        5 => Bzhi,
        6 => Pdep,
        _ => Pext,
    };
    let (r, cf) = x86jit_core::interp::bmi_result(a, b, size as u8, bmiop);
    *out = r;
    *out.add(1) = cf as u64;
}

/// EVEX masked move helper (task-170.1): runs the shared `CpuState::write_masked`, so
/// the JIT's masking is bit-identical to the interpreter's (masked ops aren't hot, so
/// a helper call beats hand-emitting a per-lane blend). Args are widened to u64.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vmaskmov_helper(
    cpu: *mut u8,
    dst: u64,
    src: u64,
    k: u64,
    elem: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let newval = cpu.vec_lanes(src as usize);
    cpu.write_masked(
        dst as usize,
        newval,
        k as u8,
        elem as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// EVEX write-masked vector **memory** move `vmovdqu{8,16,32,64} v{k}{z}, [mem]` (load)
/// and `[mem]{k}, v` (store) (task-168.5.5). Element-wise via the shared
/// `masked_load_run`/`masked_store_run` so masked-off lanes never fault (hardware
/// suppression) and JIT == interpreter. On an active-lane fault, writes the fault into
/// `MemCtx` (RIP already set by the shared fn) and returns `RET_UNMAPPED`; else
/// `RET_CONTINUE`. Uses the bounds-only `RawStrMem` view, like `string_helper`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vmaskmov_mem_helper(
    cpu: *mut u8,
    mem: *mut u8,
    reg: u64,
    addr: u64,
    k: u64,
    elem: u64,
    zeroing: u64,
    bytes: u64,
    is_store: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    let km = cpu.kmask[k as usize];
    let fault = if is_store != 0 {
        x86jit_core::interp::masked_store_run(
            cpu,
            &raw,
            reg as u8,
            addr,
            km,
            elem as u8,
            bytes as u16,
            cur_addr,
        )
    } else {
        x86jit_core::interp::masked_load_run(
            cpu,
            &raw,
            reg as u8,
            addr,
            km,
            elem as u8,
            zeroing != 0,
            bytes as u16,
            cur_addr,
        )
    };
    match fault {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// Masked EVEX logic (task-168.5.5): compute `op(a, b)` then masked-write into `dst`,
/// via the shared `exec_masked_logic` so JIT == interpreter.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vmasked_logic_helper(
    cpu: *mut u8,
    op_code: u64,
    dst: u64,
    a: u64,
    b: u64,
    k: u64,
    elem: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_masked_logic(
        cpu,
        op_code as u8,
        dst as u8,
        a as u8,
        b as u8,
        k as u8,
        elem as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// SSE4.2 `pcmpistri`/`pcmpestri` (task-168.5.4): the string-aggregation index + flags,
/// via the shared `pcmpstr_run`. Writes `out[0] = ecx`, `out[1] = cf|zf<<1|sf<<2|of<<3`;
/// the codegen stores ECX and the flags through its own GPR/flag machinery.
///
/// # Safety
/// `cpu` is a valid `CpuState` for the call; `out` points at two writable `u64`s.
unsafe extern "C" fn pcmpstr_helper(
    cpu: *const u8,
    a: u64,
    b: u64,
    imm: u64,
    explicit: u64,
    out: *mut u64,
) {
    let cpu = &*(cpu as *const x86jit_core::state::CpuState);
    let (ecx, cf, zf, sf, of) =
        x86jit_core::interp::pcmpstr_run(cpu, a as u8, b as u8, imm as u8, explicit != 0);
    *out = ecx as u64;
    *out.add(1) = (cf as u64) | ((zf as u64) << 1) | ((sf as u64) << 2) | ((of as u64) << 3);
}

/// EVEX `valign{d,q}` (task-168.5.6): cross-lane element shift, via the shared
/// `exec_valign` so JIT == interpreter.
unsafe extern "C" fn valign_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    b: u64,
    shift: u64,
    elem: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_valign(
        cpu,
        dst as u8,
        a as u8,
        b as u8,
        shift as u8,
        elem as u8,
        bytes as u16,
    );
}

/// Bounded background-compile queue depth (bg-tier, doc-27 D4): a full queue makes
/// `tier_up_async` return `Busy` and the block stays interpreted — never an inline
/// compile spike under peak pressure.
const TIER_QUEUE_CAP: usize = 64;

/// Which *host* instruction set Cranelift may emit for the guest IR (task-175) — the
/// host-codegen axis, orthogonal to the guest ISA (`GuestCpuFeatures`, task-169).
/// Lives on [`JitBackend`], not `VmConfig`: the interpreter has no codegen target (it
/// is plain Rust fixed at compile time), so this would be meaningless on the shared
/// config. Guest-invisible — only instruction selection changes, not results — so the
/// interpreter stays the reference oracle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostTarget {
    /// Detect the running host and use all its features (`cranelift_native`) — the
    /// default, and the pre-task-175 behavior. A hot loop tiers into host-optimal code.
    #[default]
    Native,
    /// Forbid AVX and above (AVX2/AVX-512/FMA): deterministic, AOT-cacheable output
    /// that runs on any SSE4.1 host. Guest AVX/AVX-512 still executes — Cranelift
    /// lowers our 128-bit-lane IR to SSE.
    Baseline,
}

/// The JIT backend. Injected into a `Vm` via `Vm::with_backend` (§4.1) — the core
/// never names this type. Owns the executable-memory arena (`JITModule`) and
/// Cranelift context behind a `Mutex`, so `materialize(&self)` stays `Send + Sync`
/// for a shared `Vm`. With background tier-up (doc-27 D3) it also owns a compiler
/// worker thread and the queues feeding it.
pub struct JitBackend {
    shared: Arc<Shared>,
    /// The background compiler thread (bg-tier, doc-27 D3), spawned lazily on the
    /// first [`tier_up_async`](Backend::tier_up_async) and joined on `Drop`. `None`
    /// until a background tier-up is first requested — eager/sync use never spawns.
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// State shared between the vcpu threads (foreground `materialize`, submit, drain)
/// and the background compiler worker (bg-tier, doc-27 D3). Behind `Arc` so a
/// [`TierUpHandle`] clone can expose `wait_idle` without owning the worker thread.
struct Shared {
    inner: Mutex<Jit>,
    offsets: CpuOffsets,
    /// Superblock caps (§12 M5-T3), or `None` to compile one block at a time.
    caps: Option<RegionCaps>,
    /// Submitted-but-not-completed requests + worker coordination. The worker sleeps
    /// on `work_cv`; `wait_idle` sleeps on `idle_cv`.
    queue: Mutex<Queue>,
    work_cv: Condvar,
    idle_cv: Condvar,
    /// Finished compiles awaiting the core dispatcher's drain (decision-5).
    done: Mutex<Vec<TierUpFinished>>,
    /// Lock-free "anything to drain?" probe, kept equal to `done.len()` under the
    /// `done` lock — lets `tier_up_finished` early-out without locking.
    ready: AtomicUsize,
    /// Total nanoseconds spent in `compile_with` (every foreground / tier-up / bg
    /// compile), for the bench's compile-vs-run split (perf-bench v2, PB-2). Relaxed:
    /// a monotone accumulator, not a synchronization channel.
    compile_ns: AtomicU64,
}

/// The background compile queue and its liveness counters (bg-tier, doc-27 D3/D4).
struct Queue {
    items: VecDeque<TierUpRequest>,
    /// Requests submitted but not yet completed (queued + the one compiling).
    /// `wait_idle` blocks until this reaches zero.
    outstanding: usize,
    /// Set by `Drop` to unblock and stop the worker.
    shutdown: bool,
    /// Test lever (bg-tier BGT-4): while true the worker parks without popping, so
    /// requests pile up in the queue. Toggled via `TierUpHandle::pause_compiler`;
    /// `shutdown` still wins so `Drop` never hangs. Never set in production.
    paused: bool,
}

struct Jit {
    module: JITModule,
    fbctx: FunctionBuilderContext,
    next_id: u64,
    // Link slots for block chaining (§12 M5). Each `Box<AtomicU64>` holds a
    // compiled entry pointer (0 = unlinked); its heap address is baked into the
    // code and filled by the dispatcher. Owned here so it lives as long as the Vm.
    // The `Box` is load-bearing: a bare `Vec` would move its elements on growth,
    // invalidating the addresses already baked into compiled code.
    //
    // `AtomicU64` (not plain `u64`): the dispatcher fill and the SMC-driven clear
    // (`invalidate_links`, R1) both store atomically, so a vcpu reading the slot
    // from compiled code sees 0 or a valid entry, never a torn value. Compiled-code
    // loads are plain machine loads (aligned u64 is naturally atomic on the hosts
    // we target); only the Rust-side writes need the atomic type.
    #[allow(clippy::vec_box)]
    slots: Vec<Box<AtomicU64>>,
}

impl JitBackend {
    pub fn new() -> Self {
        Self::build(None, HostTarget::Native)
    }

    /// A JIT that forms superblocks (§12 M5-T3): the dispatcher lifts a region and
    /// compiles it as one function, up to `caps`. Opt-in until M5-T3f flips the
    /// default on.
    pub fn with_superblocks(caps: RegionCaps) -> Self {
        Self::build(Some(caps), HostTarget::Native)
    }

    /// A JIT pinned to a [`HostTarget`] (task-175): which *host* instructions Cranelift
    /// may emit for the guest IR — a separate axis from the guest ISA
    /// (`GuestCpuFeatures`, task-169). Default is [`HostTarget::Native`] (detect the
    /// running host). Guest-invisible: the emitted code is bit-identical in effect
    /// (only instruction *selection* changes), so interp == JIT holds regardless.
    pub fn with_host_target(target: HostTarget) -> Self {
        Self::build(None, target)
    }

    fn build(caps: Option<RegionCaps>, target: HostTarget) -> Self {
        let mut flags = settings::builder();
        flags.set("use_colocated_libcalls", "false").unwrap();
        flags.set("is_pic", "false").unwrap();
        let mut isa_builder = cranelift_native::builder().expect("host ISA");
        if target == HostTarget::Baseline {
            // Pin below the host: forbid AVX and above so codegen is deterministic and
            // portable to any SSE4.1 host (an AOT cache built here runs on older CPUs).
            // Cranelift lowers our 128-bit-lane vector IR to SSE, so guest AVX/AVX-512
            // still executes correctly — just via SSE host instructions. FMA off too:
            // no mul+add contraction, so results stay bit-identical to the interpreter.
            for flag in [
                "has_avx",
                "has_avx512bitalg",
                "has_avx512dq",
                "has_avx512f",
                "has_avx512vbmi",
                "has_avx512vl",
                "has_fma",
            ] {
                let _ = isa_builder.set(flag, "false");
            }
        }
        let isa = isa_builder
            .finish(settings::Flags::new(flags))
            .expect("finish ISA");
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol("x86jit_div", div_helper as *const u8);
        builder.symbol("x86jit_string", string_helper as *const u8);
        builder.symbol("x86jit_cpuid", cpuid_helper as *const u8);
        builder.symbol("x86jit_xgetbv", xgetbv_helper as *const u8);
        builder.symbol("x86jit_vmaskmov", vmaskmov_helper as *const u8);
        builder.symbol("x86jit_vmasked_logic", vmasked_logic_helper as *const u8);
        builder.symbol("x86jit_valign", valign_helper as *const u8);
        builder.symbol("x86jit_pcmpstr", pcmpstr_helper as *const u8);
        builder.symbol("x86jit_bmi", bmi_helper as *const u8);
        builder.symbol("x86jit_x87", x87_helper as *const u8);
        builder.symbol("x86jit_fxstate", fxstate_helper as *const u8);
        builder.symbol("x86jit_crc32", crc32_helper as *const u8);
        let module = JITModule::new(builder);

        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Jit {
                    module,
                    fbctx: FunctionBuilderContext::new(),
                    next_id: 0,
                    slots: Vec::new(),
                }),
                offsets: cpu_offsets(),
                caps,
                queue: Mutex::new(Queue {
                    items: VecDeque::new(),
                    outstanding: 0,
                    shutdown: false,
                    paused: false,
                }),
                work_cv: Condvar::new(),
                idle_cv: Condvar::new(),
                done: Mutex::new(Vec::new()),
                ready: AtomicUsize::new(0),
                compile_ns: AtomicU64::new(0),
            }),
            worker: Mutex::new(None),
        }
    }

    /// Spawn the background compiler thread if it isn't running yet (bg-tier, doc-27
    /// D3). Lazy: eager/sync-only use never reaches here, so it never spawns.
    fn ensure_worker(&self) {
        let mut w = self.worker.lock().unwrap();
        if w.is_none() {
            let shared = Arc::clone(&self.shared);
            *w = Some(
                std::thread::Builder::new()
                    .name("x86jit-tier".into())
                    .spawn(move || shared.worker_loop())
                    .expect("spawn tier-up worker"),
            );
        }
    }

    /// A handle to the background tier-up machinery (bg-tier, doc-27 D6). Its
    /// [`wait_idle`](TierUpHandle::wait_idle) blocks until every submitted compile
    /// has completed — the determinism lever for tests. Grab it before boxing the
    /// backend into a `Vm`.
    pub fn tier_up_handle(&self) -> TierUpHandle {
        TierUpHandle {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Shared {
    fn compile(
        &self,
        ir: &IrBlock,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CompiledPtr {
        self.compile_with(|builder, helpers, alloc_slot| {
            codegen::translate_block(
                builder,
                ir,
                &self.offsets,
                alloc_slot,
                helpers,
                consistency,
                mmio,
                guest_base,
            );
        })
    }

    /// Compile a superblock region (§12 M5-T3) as one function.
    fn compile_region(
        &self,
        region: &IrRegion,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CompiledPtr {
        self.compile_with(|builder, helpers, alloc_slot| {
            codegen::translate_region(
                builder,
                region,
                &self.offsets,
                alloc_slot,
                helpers,
                consistency,
                mmio,
                guest_base,
            );
        })
    }

    /// Shared function-building spine: sets up the signature, imports the five
    /// helpers, runs `translate` to emit the body, and finalizes. `translate`
    /// receives the builder, the imported helper refs, and the link-slot allocator.
    fn compile_with(
        &self,
        translate: impl FnOnce(&mut FunctionBuilder, codegen::Helpers, &mut dyn FnMut() -> u64),
    ) -> CompiledPtr {
        let started = Instant::now();
        let mut jit = self.inner.lock().unwrap();
        jit.next_id += 1;
        let name = format!("blk_{}", jit.next_id);

        let mut ctx = jit.module.make_context();
        let ptr = jit.module.target_config().pointer_type();
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.returns.push(AbiParam::new(types::I64));

        // Six Rust helpers, reached from compiled code by `call_indirect` through
        // their baked absolute address rather than a linker-relocated direct call —
        // so the emitted machine code carries no relocations (the prerequisite for a
        // persistable AOT code cache; see backlog/docs/design/aot-plan.md). Build each
        // signature here; `import_signature` + the fn address are wired into
        // `Helpers` below, inside the builder scope.
        // `n` I64 params, plus an I64 return when `ret` (the value-returning helpers);
        // the void helpers (cpuid/xgetbv/vmaskmov/bmi write through a `cpu`/`out` pointer).
        let params = |n: usize, ret: bool| {
            let mut s = jit.module.make_signature();
            for _ in 0..n {
                s.params.push(AbiParam::new(types::I64));
            }
            if ret {
                s.returns.push(AbiParam::new(types::I64));
            }
            s
        };
        let div_sig = params(6, true); // div(hi, lo, divisor, size, signed, out) -> i64
        let str_sig = params(6, true); // string(cpu, mem, op, elem, rep, cur_addr) -> i64
        let x87_sig = params(6, true); // x87(cpu, mem, kind, addr, sti, cur_addr) -> i64
        let fx_sig = params(5, true); // fxstate(cpu, mem, addr, restore, cur_addr) -> i64
        let crc_sig = params(3, true); // crc32(crc, src, bytes) -> i64
        let cpuid_sig = params(1, false); // cpuid(cpu) -> ()
        let xgetbv_sig = params(1, false); // xgetbv(cpu) -> ()
        let vmaskmov_sig = params(7, false); // vmaskmov(cpu, dst, src, k, elem, zeroing, bytes) -> ()
        let vmaskmov_mem_sig = params(10, true); // (cpu, mem, reg, addr, k, elem, zeroing, bytes, is_store, cur_addr) -> i64
        let vmasked_logic_sig = params(9, false); // (cpu, op, dst, a, b, k, elem, zeroing, bytes) -> ()
        let valign_sig = params(7, false); // valign(cpu, dst, a, b, shift, elem, bytes) -> ()
        let pcmpstr_sig = params(6, false); // pcmpstr(cpu, a, b, imm, explicit, out) -> ()
        let bmi_sig = params(5, false); // bmi(a, b, op, size, out) -> () — result + CF via `out`

        {
            let Jit { fbctx, slots, .. } = &mut *jit;
            let mut alloc_slot = || {
                let b = Box::new(AtomicU64::new(0));
                let addr = &*b as *const AtomicU64 as u64;
                slots.push(b);
                addr
            };
            let mut builder = FunctionBuilder::new(&mut ctx.func, fbctx);
            // Each helper is `(imported signature ref, baked fn address)`.
            macro_rules! helper {
                ($sig:expr, $f:expr) => {
                    (builder.import_signature($sig), $f as *const u8 as u64)
                };
            }
            let helpers = codegen::Helpers {
                div: helper!(div_sig, div_helper),
                string: helper!(str_sig, string_helper),
                cpuid: helper!(cpuid_sig, cpuid_helper),
                xgetbv: helper!(xgetbv_sig, xgetbv_helper),
                vmaskmov: helper!(vmaskmov_sig, vmaskmov_helper),
                vmaskmov_mem: helper!(vmaskmov_mem_sig, vmaskmov_mem_helper),
                vmasked_logic: helper!(vmasked_logic_sig, vmasked_logic_helper),
                valign: helper!(valign_sig, valign_helper),
                pcmpstr: helper!(pcmpstr_sig, pcmpstr_helper),
                bmi: helper!(bmi_sig, bmi_helper),
                x87: helper!(x87_sig, x87_helper),
                fxstate: helper!(fx_sig, fxstate_helper),
                crc32: helper!(crc_sig, crc32_helper),
            };
            translate(&mut builder, helpers, &mut alloc_slot);
            builder.finalize();
        }

        let id = jit
            .module
            .declare_function(&name, Linkage::Export, &ctx.func.signature)
            .expect("declare function");
        jit.module
            .define_function(id, &mut ctx)
            .expect("define function");
        // GP-3 (doc-30): capture the code size + sorted `(host_off, guest_rip)`
        // srcloc table before `clear_context` wipes it, to register in the
        // process-global `CodeMap` once the host entry address is known below.
        let (code_len, srcloc_table) = {
            let cc = ctx.compiled_code().expect("compiled code present");
            let table: Vec<(u32, u32)> = cc
                .buffer
                .get_srclocs_sorted()
                .iter()
                .filter(|s| !s.loc.is_default())
                .map(|s| (s.start, s.loc.bits()))
                .collect();
            (cc.code_info().total_size, table.into_boxed_slice())
        };
        jit.module.clear_context(&mut ctx);
        jit.module.finalize_definitions().expect("finalize");

        let entry = CompiledPtr(jit.module.get_finalized_function(id));
        x86jit_core::codemap::register(entry.0 as usize, code_len, srcloc_table);
        // Account this compile's wall-time for the bench compile-vs-run split (PB-2).
        // Includes the `inner` lock wait — that contention is real compile-path cost.
        self.compile_ns
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        entry
    }

    /// Compile one background request's unit (bg-tier, doc-27 D3): a single block, or
    /// (BGT-6) a hotness-gated superblock region — same off-thread path either way.
    fn compile_request(&self, req: &TierUpRequest) -> CompiledPtr {
        match &req.unit {
            TierUpUnit::Block(ir) => self.compile(ir, req.consistency, req.mmio, req.guest_base),
            TierUpUnit::Region(region) => {
                self.compile_region(region, req.consistency, req.mmio, req.guest_base)
            }
        }
    }

    /// The background compiler loop (bg-tier, doc-27 D3): pull a request, compile it
    /// under the shared JIT mutex (so `JITModule`'s `!Sync`/`&mut finalize` is
    /// satisfied exactly as the foreground path, serialized against it), publish the
    /// result to `done`, and repeat until `Drop` sets `shutdown`.
    fn worker_loop(&self) {
        loop {
            let req = {
                let mut q = self.queue.lock().unwrap();
                // Park while empty or paused; `shutdown` always wins so `Drop` joins.
                while (q.items.is_empty() || q.paused) && !q.shutdown {
                    q = self.work_cv.wait(q).unwrap();
                }
                if q.shutdown && q.items.is_empty() {
                    break;
                }
                q.items.pop_front().expect("non-empty queue")
            };
            // Compile OUTSIDE the queue lock (it takes `inner`); a concurrent submit
            // or foreground `materialize` isn't blocked by this block's compile.
            let entry = self.compile_request(&req);
            {
                let mut done = self.done.lock().unwrap();
                done.push(TierUpFinished {
                    pc: req.pc,
                    block: CachedBlock::Compiled { entry },
                    spans: req.spans.clone(),
                    epoch: req.epoch,
                });
                // Keep the lock-free probe equal to `done.len()` (set under the lock).
                self.ready.store(done.len(), Ordering::Release);
            }
            let mut q = self.queue.lock().unwrap();
            q.outstanding -= 1;
            if q.outstanding == 0 {
                self.idle_cv.notify_all();
            }
        }
    }
}

/// A cloneable handle to a [`JitBackend`]'s background tier-up machinery (bg-tier,
/// doc-27 D6), exposing `wait_idle` for deterministic tests without owning the
/// worker thread.
pub struct TierUpHandle {
    shared: Arc<Shared>,
}

impl TierUpHandle {
    /// Block until every submitted background compile has completed and its result
    /// is queued for drain (the completions are then observable via
    /// `Backend::tier_up_finished`). No sleeps — waits on the worker's idle signal.
    pub fn wait_idle(&self) {
        let mut q = self.shared.queue.lock().unwrap();
        while q.outstanding > 0 {
            q = self.shared.idle_cv.wait(q).unwrap();
        }
    }

    /// Test lever (bg-tier BGT-4): park the background worker so queued requests pile
    /// up uncompiled until the returned guard drops — lets a race test line up several
    /// in-flight requests for one pc before any of them lands. Sets a queue flag (it
    /// does NOT hold the compiler mutex, so a vcpu can still invalidate/`materialize`
    /// on the same thread). Not for production use.
    pub fn pause_compiler(&self) -> CompilerPause {
        self.shared.queue.lock().unwrap().paused = true;
        CompilerPause {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Guard from [`TierUpHandle::pause_compiler`]: releasing it unparks the worker.
pub struct CompilerPause {
    shared: Arc<Shared>,
}

impl Drop for CompilerPause {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().paused = false;
        self.shared.work_cv.notify_all();
    }
}

impl Default for JitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for JitBackend {
    fn materialize(
        &self,
        ir: &IrBlock,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CachedBlock {
        CachedBlock::Compiled {
            entry: self.shared.compile(ir, consistency, mmio, guest_base),
        }
    }

    fn region_caps(&self) -> Option<RegionCaps> {
        self.shared.caps
    }

    fn materialize_region(
        &self,
        region: &IrRegion,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CachedBlock {
        CachedBlock::Compiled {
            entry: self
                .shared
                .compile_region(region, consistency, mmio, guest_base),
        }
    }

    fn invalidate_links(&self) {
        // Zero every link slot so no surviving block chains into a unit an SMC
        // invalidation just dropped (R1). Over-invalidation (all slots, not only
        // the victims') is deliberate: invalidation is rare, and a cleared slot
        // simply re-links via `RET_LINK` on its next traversal. Relaxed stores pair
        // with the dispatcher's relaxed fill; compiled-code reads see 0 or a valid
        // entry. Runs under the compiler mutex, off the hot path.
        let jit = self.shared.inner.lock().unwrap();
        for slot in &jit.slots {
            slot.store(0, Ordering::Relaxed);
        }
    }

    fn tier_up_async(&self, req: TierUpRequest) -> TierUpSubmit {
        self.ensure_worker();
        let mut q = self.shared.queue.lock().unwrap();
        if q.shutdown {
            // Racing `Drop` — decline; the caller stays interpreted (correct, slow).
            return TierUpSubmit::Unsupported;
        }
        if q.items.len() >= TIER_QUEUE_CAP {
            // Backpressure: never compile inline in response (doc-27 D1).
            return TierUpSubmit::Busy;
        }
        q.items.push_back(req);
        q.outstanding += 1;
        drop(q);
        self.shared.work_cv.notify_one();
        TierUpSubmit::Queued
    }

    fn tier_up_finished(&self) -> Vec<TierUpFinished> {
        // Fast path: nothing published since the last drain — no lock, no alloc.
        if self.shared.ready.load(Ordering::Acquire) == 0 {
            return Vec::new();
        }
        let mut done = self.shared.done.lock().unwrap();
        self.shared.ready.store(0, Ordering::Release);
        std::mem::take(&mut *done)
    }

    fn compile_ns(&self) -> u64 {
        self.shared.compile_ns.load(Ordering::Relaxed)
    }
}

impl Drop for JitBackend {
    fn drop(&mut self) {
        // Signal shutdown and wake the worker so no thread outlives the module it
        // compiles into (use-after-free guard). A worker that panicked poisons the
        // queue mutex — recover the guard and still join; never re-panic in `Drop`
        // (a dead worker just means blocks stay interpreted: slow, not unsound).
        {
            let mut q = self.shared.queue.lock().unwrap_or_else(|p| p.into_inner());
            q.shutdown = true;
        }
        self.shared.work_cv.notify_all();
        if let Some(handle) = self
            .worker
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = handle.join(); // swallow a worker panic
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86jit_core::jit_abi::run_compiled;
    use x86jit_core::lift::lift_block;
    use x86jit_core::{CpuState, Memory, MemoryModel, Prot, RegionKind, StepResult};

    // `mov eax, 42` then `jmp $` (to self): sets RAX=42 and terminates the block
    // without touching guest memory (the jmp is the block terminator, not a loop).
    const CODE: &[u8] = &[0xb8, 0x2a, 0x00, 0x00, 0x00, 0xeb, 0xf9];
    const ENTRY: u64 = 0x1000;

    fn mem_with_code() -> Memory {
        let mut m = Memory::new(MemoryModel::Flat { size: 0x4000 });
        m.map(ENTRY, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
        m.write_bytes(ENTRY, CODE).unwrap();
        m
    }

    fn request(mem: &Memory) -> TierUpRequest {
        let ir = lift_block(mem, ENTRY).expect("lift the block");
        TierUpRequest {
            pc: ENTRY,
            unit: TierUpUnit::Block(Arc::new(ir)),
            consistency: MemConsistency::Fast,
            mmio: None,
            guest_base: 0,
            spans: vec![(ENTRY, CODE.len() as u32)],
            epoch: 0,
        }
    }

    /// Run a compiled block from a fresh CPU and return RAX (should be 42).
    fn run_rax(entry: CompiledPtr, mem: &Memory) -> u64 {
        let mut cpu = CpuState::new();
        cpu.rip = ENTRY;
        let step = unsafe { run_compiled(entry, &mut cpu, mem) };
        assert!(matches!(step, StepResult::Continue), "block continues");
        cpu.gpr[0]
    }

    fn compiled_entry(block: CachedBlock) -> CompiledPtr {
        match block {
            CachedBlock::Compiled { entry } => {
                assert!(!entry.0.is_null(), "compiled entry is non-null");
                entry
            }
            CachedBlock::Interpreted(_) => panic!("expected a compiled block"),
        }
    }

    /// AC#1: a background-compiled block is drained as a `Compiled` unit and runs
    /// correctly (RAX=42), matching the eager path.
    #[test]
    fn background_compile_runs_correctly() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let handle = jit.tier_up_handle();

        assert_eq!(jit.tier_up_async(request(&mem)), TierUpSubmit::Queued);
        handle.wait_idle();

        let mut done = jit.tier_up_finished();
        assert_eq!(done.len(), 1, "exactly one completion drained");
        let fin = done.pop().unwrap();
        assert_eq!(fin.pc, ENTRY);
        assert_eq!(fin.spans, vec![(ENTRY, CODE.len() as u32)]);
        assert_eq!(fin.epoch, 0);
        assert_eq!(run_rax(compiled_entry(fin.block), &mem), 42);

        // Drained: the fast probe is clear and a second drain is empty.
        assert!(jit.tier_up_finished().is_empty());
    }

    /// AC#5: no worker thread is spawned until the first `tier_up_async`.
    #[test]
    fn worker_spawns_lazily() {
        let jit = JitBackend::new();
        assert!(
            jit.worker.lock().unwrap().is_none(),
            "no thread before any tier-up request"
        );
        let mem = mem_with_code();
        let _ = jit.tier_up_async(request(&mem));
        assert!(
            jit.worker.lock().unwrap().is_some(),
            "the first request spawns the worker"
        );
    }

    /// AC#2: a full queue returns `Busy` (never an inline compile), and the queued
    /// requests all complete once the compiler is unblocked. The worker is stalled
    /// by holding the JIT mutex it needs, so the queue provably fills.
    #[test]
    fn busy_on_full_queue_then_queued_complete() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let handle = jit.tier_up_handle();

        let guard = jit.shared.inner.lock().unwrap(); // stall every compile
        let mut queued = 0usize;
        let mut busy = false;
        for _ in 0..(TIER_QUEUE_CAP + 8) {
            match jit.tier_up_async(request(&mem)) {
                TierUpSubmit::Queued => queued += 1,
                TierUpSubmit::Busy => busy = true,
                TierUpSubmit::Unsupported => panic!("unexpected Unsupported"),
            }
        }
        assert!(busy, "a full queue must report Busy");
        assert!(queued >= TIER_QUEUE_CAP, "queue filled to capacity");

        drop(guard); // unblock the worker
        handle.wait_idle();
        assert_eq!(
            jit.tier_up_finished().len(),
            queued,
            "every queued request completed"
        );
    }

    /// AC#4: the eager/foreground `materialize` still works (correct output, no
    /// deadlock) while the worker churns a backlog — both take the same JIT mutex.
    #[test]
    fn eager_materialize_works_while_worker_busy() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let handle = jit.tier_up_handle();

        for _ in 0..30 {
            let _ = jit.tier_up_async(request(&mem));
        }
        // Foreground compile the same block amid the backlog.
        let ir = lift_block(&mem, ENTRY).unwrap();
        let entry = compiled_entry(jit.materialize(&ir, MemConsistency::Fast, None, 0));
        assert_eq!(run_rax(entry, &mem), 42);

        handle.wait_idle();
        for fin in jit.tier_up_finished() {
            assert_eq!(run_rax(compiled_entry(fin.block), &mem), 42);
        }
    }

    /// AC#3a: dropping with requests queued/mid-compile joins the worker cleanly
    /// (the test hangs on a leaked thread, panics on a double-free — neither here).
    #[test]
    fn drop_joins_with_work_queued() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        for _ in 0..20 {
            let _ = jit.tier_up_async(request(&mem));
        }
        drop(jit); // must join without hanging
    }

    /// AC#3b: a poisoned queue mutex (a stand-in for a panicked worker) must not
    /// make `Drop` re-panic — a dead worker only means blocks stay interpreted.
    #[test]
    fn drop_survives_poisoned_mutex() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let _ = jit.tier_up_async(request(&mem)); // spawn the worker
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = jit.shared.queue.lock().unwrap();
            panic!("poison the queue mutex");
        }));
        drop(jit); // must not re-panic despite the poisoned mutex
    }
}
