//! `Vm` (shared) and `Vcpu` (per-thread) — the KVM-style split (§2), plus the
//! dispatcher loop (§9.2).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::cache::{BlockKey, CachedBlock, CompiledPtr, TranslationCache};
use crate::exit::{AccessKind, Exit, StepResult};
use crate::ir::{IrBlock, IrRegion, RegionCaps};
use crate::jit_abi::{
    call_block, MemCtx, RetStack, RET_CHAIN, RET_CONTINUE, RET_EXCEPTION, RET_HLT, RET_IBTC_MISS,
    RET_LINK, RET_MMIO_DEFER, RET_PORTIO_DEFER, RET_SYSCALL, RET_UNMAPPED,
};
use crate::lift::{lift_block, lift_region, CpuMode, FetchAddr, LiftError};
use crate::memory::{HostRam, MapError, MemError, Memory, MemoryModel, Prot, RegionKind};
use crate::state::{CpuState, Flags, Reg};

/// Materializes IR into an executable `CachedBlock` (§8). The ONLY
/// backend-dependent operation; execution is uniform.
///
/// Injected as a trait object (§4.1) — NOT a config enum. The core can't name
/// the downstream JIT crate (dependency points the other way), so an
/// `enum Backend { Interpreter, Jit }` is unbuildable. The interpreter impl lives
/// here; `x86jit-cranelift` exports a `JitBackend` implementing this same trait
/// and the user injects it via `Vm::with_backend`.
///
/// `materialize` takes `&self` (not `&mut self`) so a `Vm` can be shared across
/// vcpus behind `Arc`. A JIT impl that needs a mutable compiler context wraps it
/// in interior mutability (e.g. `Mutex`).
pub trait Backend: Send + Sync {
    /// Compile `ir` for the given consistency tier. `consistency` only affects a
    /// JIT on a weak host (it picks the barrier strategy for ordinary guest
    /// loads/stores, §8.2.3); the interpreter and x86 hosts ignore it.
    /// `mmio` is the guest's `Trap`-region window (`[lo, hi)`), or `None` if the VM
    /// has no MMIO regions (§5.2, M4-T10). A JIT bakes it as a compile-time constant
    /// and, when `Some`, adds a per-access range check that defers a Trap-region
    /// load/store to the interpreter; `None` means no check and zero overhead.
    /// `guest_base` is the guest address the RAM buffer's first byte represents (§4.1,
    /// identity mapping). A JIT bakes it as a compile-time constant: `0` (the common
    /// case) emits the historical `host = base + guest_addr`; a non-zero base emits the
    /// rebased `host = base + (guest_addr - guest_base)` plus a lower-bound reject, so
    /// the zero-base hot path is byte-identical.
    fn materialize(
        &self,
        ir: &IrBlock,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CachedBlock;

    /// Superblock caps if this backend forms regions (§12 M5-T3), else `None`
    /// (the default). When `Some`, the dispatcher lifts a region and calls
    /// [`materialize_region`](Backend::materialize_region); a one-block region
    /// falls back to `materialize`.
    fn region_caps(&self) -> Option<RegionCaps> {
        None
    }

    /// Compile a multi-block region into one unit. Only called when
    /// [`region_caps`](Backend::region_caps) is `Some`; the default is unreachable.
    fn materialize_region(
        &self,
        _region: &IrRegion,
        _consistency: MemConsistency,
        _mmio: Option<(u64, u64)>,
        _guest_base: u64,
    ) -> CachedBlock {
        unreachable!("materialize_region called on a backend without region_caps")
    }

    /// Invalidate every backend-owned cached code pointer (link slots, and later
    /// IBTC / return-continuation slots) after an SMC invalidation dropped one or
    /// more compiled units (fast dispatch R1). The default is a no-op (the
    /// interpreter has no such state). A JIT clears all its slots: a cleared slot
    /// re-links via the existing `RET_LINK` path on the next traversal, so
    /// over-invalidation is safe and avoids a reverse target→slot index. Called
    /// only when [`TranslationCache::invalidate_overlapping`] actually drops a unit,
    /// which is rare (a write landing on a code page).
    fn invalidate_links(&self) {}

    /// Submit a hot block for **background** compilation off the vcpu's critical
    /// path (bg-tier, doc-27 D1). The default is [`TierUpSubmit::Unsupported`] — a
    /// backend that doesn't run a compiler thread (the interpreter, or the JIT with
    /// background tier-up disabled) never queues, and the dispatcher falls back to
    /// its existing inline/eager path. A backend that accepts the work returns
    /// [`TierUpSubmit::Queued`]; [`TierUpSubmit::Busy`] means "queue full, stay
    /// interpreted and retry" — never an inline compile spike under peak pressure.
    /// Takes `&self` (like [`materialize`](Backend::materialize)) — the compiler
    /// state is interior-mutable. **Inert until BGT-3 wires the call site.**
    fn tier_up_async(&self, _req: TierUpRequest) -> TierUpSubmit {
        TierUpSubmit::Unsupported
    }

    /// Drain finished background compiles for the core dispatcher to publish via
    /// `cache.upgrade` (decision-5: the backend never touches the cache). The
    /// default returns an empty `Vec` (no allocation) for a backend that never
    /// queues. Called at the top of the dispatch loop; each result carries the
    /// epoch snapshot taken at submit, so a stale compile is rejected on publish.
    fn tier_up_finished(&self) -> Vec<TierUpFinished> {
        Vec::new()
    }

    /// Total time spent compiling (in `materialize`/`materialize_region`) over this
    /// backend's lifetime, in nanoseconds — for the bench's compile-vs-run split
    /// (perf-bench v2, doc-29 PB-2). The default is `0`: a backend that does no
    /// compilation (the interpreter) has no compile cost to subtract. A JIT
    /// accumulates it with interior mutability. Observability only — never on the
    /// hot path.
    fn compile_ns(&self) -> u64 {
        0
    }
}

/// A hot block handed to a backend for background compilation (bg-tier, doc-27
/// D1). Plain data — no threads or channels cross the [`Backend`] boundary, so
/// `x86jit-core`'s dependency set stays `{iced-x86}` (§15). Mirrors the arguments
/// the inline tier-up already passes to [`Backend::materialize`], plus the
/// `span`/`epoch` the dispatcher needs to publish the result safely.
pub struct TierUpRequest {
    /// Guest entry address of the block/region (its cache key).
    pub pc: u64,
    /// The already-lifted IR to compile — a single block, or (BGT-6, doc-27 Phase 6)
    /// a hotness-gated superblock region compiled off-thread.
    pub unit: TierUpUnit,
    /// Consistency tier to compile for (§8.2.3).
    pub consistency: MemConsistency,
    /// The guest `Trap`-region window, baked as a constant (§5.2, M4-T10).
    pub mmio: Option<(u64, u64)>,
    /// The guest base (host addr of `ptr[0]`), baked as a constant (§4.1). `0` is the
    /// common zero-based layout; non-zero drives identity mapping.
    pub guest_base: u64,
    /// The guest byte span(s) `(start, len)` for re-establishing SMC coverage on
    /// publish — one for a block, one per sub-block for a region (matches
    /// [`TranslationCache::insert`]'s span list).
    pub spans: Vec<(u64, u32)>,
    /// Invalidation epoch snapshotted at submit; a publish is rejected if the cache
    /// epoch has moved past it (an SMC drop invalidated the unit mid-compile).
    pub epoch: u64,
}

/// The IR unit a background tier-up compiles (bg-tier, doc-27; BGT-6 adds `Region`).
/// Plain data across the [`Backend`] boundary — no cranelift types leak into the core.
pub enum TierUpUnit {
    /// A single already-lifted block (BGT-1..5).
    Block(Arc<IrBlock>),
    /// A hotness-gated multi-block superblock region (BGT-6): only proven-hot loops
    /// form one, and only off the vcpu — region compile is too heavy inline
    /// (superblock-plan.md T3f).
    Region(Arc<IrRegion>),
}

/// A finished background compile, ready for the core dispatcher to publish
/// (bg-tier, doc-27 D2 / decision-5). Carries everything `cache.upgrade` needs;
/// the backend returns these from [`Backend::tier_up_finished`] and never writes
/// the cache itself.
pub struct TierUpFinished {
    /// Guest entry address (cache key), echoing the request's `pc`.
    pub pc: u64,
    /// The compiled unit to swap in for the interpreted block.
    pub block: CachedBlock,
    /// The guest byte span(s) `(start, len)` — one per sub-block for a region.
    pub spans: Vec<(u64, u32)>,
    /// The epoch snapshotted at submit, checked against the live cache epoch.
    pub epoch: u64,
}

/// Outcome of a [`Backend::tier_up_async`] submission (bg-tier, doc-27 D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierUpSubmit {
    /// Accepted — the block will be compiled on the backend's worker thread.
    Queued,
    /// Rejected for backpressure (queue full) — stay interpreted and retry later;
    /// the dispatcher must NOT compile inline in response.
    Busy,
    /// The backend runs no background compiler (interpreter, or JIT with background
    /// tier-up off) — the dispatcher uses its existing inline/eager path.
    Unsupported,
}

/// Default backend: wrap the IR in an `Arc` and interpret it (§8.1).
pub struct InterpreterBackend;

impl Backend for InterpreterBackend {
    fn materialize(
        &self,
        ir: &IrBlock,
        _consistency: MemConsistency,
        _mmio: Option<(u64, u64)>,
        _guest_base: u64,
    ) -> CachedBlock {
        CachedBlock::Interpreted(Arc::new(ir.clone()))
    }
}

/// Memory-consistency tier for generated code on weak hosts (§4.1, §8.2.3).
/// Escalation ladder per workload: `Fast` → `AcqRel` → `FullTso`. On an x86 host
/// all tiers emit identical code (native TSO). Governs ORDINARY loads/stores only —
/// locked ops (`lock`, `xchg`) and `mfence` get real atomics/fences in every tier.
/// Distinct from `MemoryModel` (address-space layout): this is ordering.
/// Baked into compiled blocks — changing it requires flushing the translation cache.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MemConsistency {
    /// Bare STR/LDR, no barriers. Fastest. Correct only for code that doesn't
    /// synchronize through memory (single-threaded, or non-communicating threads).
    Fast,
    /// STLR / LDAPR (RCpc, ARMv8.3; LDAR fallback). The standard x86-TSO mapping;
    /// covers ~99% of correct multithreaded code (§8.2.3 theory-vs-practice note).
    AcqRel,
    /// STR+DMB ISH / LDR+DMB ISHLD. Slowest; restores store-load ordering AcqRel
    /// can miss in practice. The hammer for workloads that still misbehave.
    FullTso,
}

pub struct VmConfig {
    pub memory_model: MemoryModel,
    /// Consistency tier for weak hosts (§4.1, §8.2.3). Start: `Fast`.
    pub consistency: MemConsistency,
}

impl VmConfig {
    /// A `Flat` guest of `size` bytes at the default `Fast` consistency — the common
    /// case (task-171). Refine `consistency` on the returned value if a weak host
    /// needs a stronger tier.
    pub fn flat(size: u64) -> Self {
        VmConfig {
            memory_model: MemoryModel::Flat { size },
            consistency: MemConsistency::Fast,
        }
    }

    /// A `Reserved` (embedder-mmap'd) span of `span` bytes at `Fast` consistency
    /// (task-171). Pair with [`Vm::with_backend_host_ram`].
    pub fn reserved(span: u64) -> Self {
        VmConfig {
            memory_model: MemoryModel::Reserved { span },
            consistency: MemConsistency::Fast,
        }
    }
}

/// Shared per-machine state: guest memory + translation cache + backend (§2).
pub struct Vm {
    pub mem: Memory,
    pub cache: TranslationCache,
    pub backend: Box<dyn Backend>,
    pub consistency: MemConsistency,
    /// Hotness-gated tier-up (FD tiering): when `Some(n)`, a freshly-lifted block
    /// runs on the interpreter and is JIT-compiled only after it executes `n` times.
    /// `None` (default) keeps the eager behavior — compile every block on first
    /// sight. Cuts one-shot compile cost (run-once blocks never reach the backend)
    /// while hot loops still tier up. Only meaningful with a compiling backend.
    tier_up_after: Option<u32>,
    /// Background tier-up (bg-tier, doc-27): when true — and `tier_up_after` is
    /// `Some` with an async-capable backend — a hot block is compiled on the
    /// backend's worker thread and swapped in when ready, instead of compiling
    /// inline on the vcpu's critical path. Default false: opt-in, so the
    /// differential/fuzz corpus never depends on *when* the interp→compiled switch
    /// lands (the task-106 stance). Falls back to inline tier-up on a backend that
    /// returns `Unsupported`.
    tier_up_background: bool,
    /// Adaptive region tier-up threshold T2 (task-156): with a region-forming backend,
    /// a hot **loop** stays interpreted until it has run `Some(n)` times before tiering
    /// up to a background superblock region — a much higher bar than `tier_up_after`
    /// (T1), because a region's heavy compile only pays off on a long-running loop
    /// (measured — a premature region regresses, superblock-plan.md T3f). Non-loop
    /// blocks tier the single block at T1 as usual. `None` → use T1 (the pre-156
    /// behavior: a loop regions as soon as it's hot).
    ///
    /// **Partial (task-156 foundation).** A region-candidate loop stays *interpreted*
    /// until T2 — it does NOT take a single-block baseline tier in the meantime, so a
    /// hot-but-shorter-than-T2 loop that a region wouldn't help interprets the whole
    /// time (no baseline speedup). The production fix is a compiled-in **backedge
    /// counter** (true OSR): baseline-compile at T1, then promote the *compiled* loop to
    /// a region at T2 — a dispatcher counter can't do it because chained compiled blocks
    /// never return to the dispatcher. That's the follow-up; keep T2 `None` (or use a
    /// region-forming backend without setting T2) for the shipped, footgun-free path.
    tier_up_region_after: Option<u32>,
    /// Guest CPU feature set every new vcpu starts with (task-169). Default reproduces
    /// the historically-hardcoded advertised set (`GuestCpuFeatures::default`); an embedder
    /// selects a different ISA level via [`Vm::set_guest_cpu_features`] before spawning vcpus.
    features: crate::features::GuestCpuFeatures,
    /// x87 transcendental precision every new vcpu starts with (task-212). Default `Fast`
    /// (f64/libm); select `Extended` (full-80-bit) via [`Vm::set_x87_precision`].
    x87_precision: crate::state::X87Precision,
    /// Guest decode/lift mode (§17.3): the effective operand/address-size default every
    /// block lifts under, and part of the block-cache key (§17.4). A `Vm` is constructed
    /// in one mode (the §17 scope fence — no runtime mode switching); vcpus inherit it.
    /// Default [`CpuMode::Long64`]; select via [`Vm::set_cpu_mode`] before spawning vcpus.
    mode: CpuMode,
}

impl Vm {
    /// Construct with the default interpreter backend (lives in the core).
    pub fn new(config: VmConfig) -> Self {
        Self::with_backend(config, Box::new(InterpreterBackend))
    }

    /// Enable hotness-gated tier-up: interpret each block until it has run `n`
    /// times, then JIT-compile it. `None` restores eager compilation. Returns
    /// `self` for builder-style setup.
    pub fn set_tier_up_after(&mut self, n: Option<u32>) {
        self.tier_up_after = n;
    }

    /// Enable background tier-up (bg-tier, doc-27): a hot block is compiled off the
    /// vcpu on the backend's worker thread and swapped in when it lands, so the hot
    /// dispatch never stalls for a compile. Only meaningful together with
    /// [`set_tier_up_after`](Vm::set_tier_up_after) and a backend that runs a
    /// compiler thread; on an `Unsupported` backend it degrades to inline tier-up.
    pub fn set_tier_up_background(&mut self, on: bool) {
        self.tier_up_background = on;
    }

    /// Set the adaptive region tier-up threshold T2 (task-156): a hot loop tiers up to a
    /// background superblock region only after `Some(n)` executions — a higher bar than
    /// [`set_tier_up_after`](Vm::set_tier_up_after) (T1), so short loops never pay a
    /// wasted region compile. `None` uses T1. Only meaningful with a region-forming
    /// backend + background tier-up.
    pub fn set_tier_up_region_after(&mut self, n: Option<u32>) {
        self.tier_up_region_after = n;
    }

    /// Select the guest CPU feature set (task-169) that vcpus spawned from this VM
    /// start with — the ISA level CPUID/`xgetbv` advertise. Call before
    /// [`new_vcpu`](Vm::new_vcpu). Default is [`GuestCpuFeatures::default`] (today's set).
    /// Advertising past what the lifter executes is a documented caller risk — a guest
    /// then traps on the unimplemented instruction (a legal `Exit`).
    pub fn set_guest_cpu_features(&mut self, features: crate::features::GuestCpuFeatures) {
        self.features = features;
    }

    /// Select the x87 transcendental precision new vcpus inherit (task-212): `Fast`
    /// (f64/libm, default) or `Extended` (full-80-bit F80). Set before spawning vcpus.
    pub fn set_x87_precision(&mut self, p: crate::state::X87Precision) {
        self.x87_precision = p;
    }

    /// The guest CPU feature set new vcpus inherit.
    pub fn guest_cpu_features(&self) -> crate::features::GuestCpuFeatures {
        self.features
    }

    /// Select the guest decode/lift mode (§17.3) this Vm — and every vcpu spawned from
    /// it — runs under. Call before [`new_vcpu`](Vm::new_vcpu). Default is
    /// [`CpuMode::Long64`].
    ///
    /// `Compat32` execution semantics are being filled in on this branch
    /// (197.2 addressing, 197.3 control flow/stack); the user-facing §17.7
    /// loud-rejection lives at the loader (197.4: non-i386 ELFs refused).
    pub fn set_cpu_mode(&mut self, mode: CpuMode) {
        self.mode = mode;
    }

    /// The guest decode/lift mode new vcpus inherit (§17.3).
    pub fn cpu_mode(&self) -> CpuMode {
        self.mode
    }

    /// Fork this VM: a child with an independent deep-copy of guest memory (§4.2),
    /// a fresh translation cache, and the given backend, inheriting the consistency
    /// tier and tier-up policy. The guest-agnostic primitive behind an OS `fork` —
    /// the embedder clones the vcpu's `CpuState` (child RAX = 0) and drives the
    /// child. Knows nothing about processes, pids, or fds.
    ///
    /// `None` when the guest memory can't be deep-copied by the core — a host-backed
    /// (embedder-`mmap`ed) `Reserved` span, which only the embedder can re-allocate.
    /// The embedder surfaces that as a typed error to the guest rather than aborting.
    pub fn fork_with_backend(&self, backend: Box<dyn Backend>) -> Option<Vm> {
        Some(Vm {
            mem: self.mem.deep_copy()?,
            cache: TranslationCache::new(),
            backend,
            consistency: self.consistency,
            tier_up_after: self.tier_up_after,
            tier_up_background: self.tier_up_background,
            tier_up_region_after: self.tier_up_region_after,
            features: self.features,
            x87_precision: self.x87_precision,
            mode: self.mode,
        })
    }

    /// Assemble a fresh `Vm` around already-built guest memory — the shared body of the
    /// public constructors, holding the default tier-up policy and CPU features in one place.
    fn from_mem(mem: Memory, backend: Box<dyn Backend>, consistency: MemConsistency) -> Self {
        Self {
            mem,
            cache: TranslationCache::new(),
            backend,
            consistency,
            tier_up_after: None,
            tier_up_background: false,
            tier_up_region_after: None,
            features: crate::features::GuestCpuFeatures::default(),
            x87_precision: crate::state::X87Precision::default(),
            mode: CpuMode::Long64,
        }
    }

    /// Construct with an injected backend — this is how the JIT gets in (§4.1).
    pub fn with_backend(config: VmConfig, backend: Box<dyn Backend>) -> Self {
        Self::from_mem(
            Memory::new(config.memory_model),
            backend,
            config.consistency,
        )
    }

    /// Like [`Vm::with_backend`] but for a `Reserved` model backed by an
    /// embedder-provided host mapping (a `MAP_NORESERVE` span the core can't allocate
    /// itself; ADR-0001). `config.memory_model` should be `Reserved { span: ram.len }`.
    pub fn with_backend_host_ram(
        config: VmConfig,
        backend: Box<dyn Backend>,
        ram: HostRam,
    ) -> Self {
        Self::from_mem(
            Memory::from_host_ram(config.memory_model, ram),
            backend,
            config.consistency,
        )
    }

    pub fn map(
        &mut self,
        guest_addr: u64,
        size: usize,
        prot: Prot,
        kind: RegionKind,
    ) -> Result<(), MapError> {
        self.mem.map(guest_addr, size, prot, kind)?;
        // Mapping a Trap (MMIO) region changes the compile-time window a JIT bakes
        // into its Trap-range check (§5.2, M4-T10). Any block already compiled with
        // a narrower (or empty) window would miss the new region, so drop the whole
        // cache; MMIO regions are set up rarely (usually before execution), making
        // this near-free. `unmap` handles the shrinking case.
        if kind == RegionKind::Trap {
            self.cache.invalidate_overlapping(0, u64::MAX, || {
                self.mem.clear_all_code_pages();
            });
            self.backend.invalidate_links();
        }
        Ok(())
    }

    pub fn write_bytes(&self, guest_addr: u64, bytes: &[u8]) -> Result<(), MemError> {
        self.mem.write_bytes(guest_addr, bytes)
    }

    pub fn read_bytes(&self, guest_addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.mem.read_bytes(guest_addr, buf)
    }

    /// Register a watched guest DATA range (task-204): guest writes to it are recorded
    /// and drained by [`Self::take_dirty_ranges`], independent of SMC code-page
    /// tracking. For an embedder that caches guest-backed resources (e.g. a GPU
    /// resource cache) and re-uploads lazily on write. Zero write-path cost when nothing
    /// is watched; poll-and-drain at a frame/submit boundary under `MemConsistency::Fast`.
    pub fn watch_range(&self, guest_addr: u64, size: u64) {
        self.mem.watch_range(guest_addr, size);
    }

    /// Stop watching a guest DATA range previously passed to [`Self::watch_range`].
    pub fn unwatch_range(&self, guest_addr: u64, size: u64) {
        self.mem.unwatch_range(guest_addr, size);
    }

    /// Drain the watched ranges written since the last call, coalesced into
    /// `(guest_addr, byte_len)` (task-204). Empty and lock-free when nothing watched
    /// was written.
    pub fn take_dirty_ranges(&self) -> Vec<(u64, u64)> {
        self.mem.take_dirty_ranges()
    }

    pub fn unmap(&mut self, guest_addr: u64, size: usize) -> Result<(), MapError> {
        self.mem.unmap(guest_addr, size)?;
        // A block cached from the now-unmapped range must not stay executable (§10):
        // drop every unit overlapping it, clear the code-page tags, and flush the
        // backend's link/IBTC slots — mirroring `handle_smc`. Without this a stale
        // block runs (or a chained edge jumps into it) instead of faulting
        // `Exit::UnmappedMemory`.
        let lo = guest_addr;
        let hi = guest_addr.saturating_add(size as u64);
        let dropped = !self
            .cache
            .invalidate_overlapping(lo, hi, || {
                let last = hi.saturating_sub(1);
                for page in
                    (lo >> crate::memory::CODE_PAGE_BITS)..=(last >> crate::memory::CODE_PAGE_BITS)
                {
                    self.mem.clear_code_page(page);
                }
            })
            .is_empty();
        if dropped {
            self.backend.invalidate_links();
        }
        Ok(())
    }

    /// Materialize a lifted block via the injected backend (§8).
    fn materialize(&self, ir: &IrBlock) -> CachedBlock {
        self.backend.materialize(
            ir,
            self.consistency,
            self.mem.trap_window(),
            self.mem.guest_base(),
        )
    }

    /// Process pending self-modifying-code writes (§10): for each code page a
    /// store landed on, drop every cached block overlapping it and clear the tag
    /// (re-execution re-lifts and re-tags). No-op unless a write actually hit a
    /// code page.
    ///
    /// Complete for the interpreter, whose stores route through `Memory::write`,
    /// and for embedder writes (loader / syscall passthrough) via `write_bytes`.
    /// JIT-compiled stores write host RAM directly (§8.2.1) and are NOT observed
    /// here; nor are the baked link slots of chained blocks patched. Faithful
    /// JIT-side SMC — write-hooks or host page protection plus reverse-edge link
    /// invalidation — is the deliberately deferred "mark the host code dead" step
    /// (§10, §9.1).
    fn handle_smc(&self) {
        let mut invalidated = false;
        for page in self.mem.take_dirty_code() {
            let lo = page << crate::memory::CODE_PAGE_BITS;
            let hi = lo + (1 << crate::memory::CODE_PAGE_BITS);
            // A dropped unit's inbound link slots (in other, surviving blocks) still
            // point at its now-stale compiled code (R1). Note whether anything was
            // dropped so we can clear the backend's slots once, below.
            // The page tag is cleared *inside* `invalidate_overlapping`, under the
            // spans lock and only if no block still spans the page — so it can't race
            // a concurrent insert's mark (#12).
            invalidated |= !self
                .cache
                .invalidate_overlapping(lo, hi, || self.mem.clear_code_page(page))
                .is_empty();
        }
        // Clear all backend-owned cached code pointers so no surviving block chains
        // into a dropped unit. Cleared slots re-link on their next traversal.
        if invalidated {
            self.backend.invalidate_links();
        }
    }

    /// One execution context per guest thread (§4.3). Shares this `Vm`.
    pub fn new_vcpu(&self) -> Vcpu {
        let mut cpu = CpuState::new();
        cpu.features = self.features; // ISA level the embedder chose (task-169)
        cpu.x87_precision = self.x87_precision; // transcendental precision (task-212)
        Vcpu {
            cpu,
            mode: self.mode, // decode/lift mode the embedder chose (§17.3)
            fast: Box::new(
                [FastEntry {
                    rip: 0,
                    entry: CompiledPtr(std::ptr::null()),
                }; FAST_N],
            ),
            fast_epoch: self.cache.epoch(),
            ibtc_refills: HashMap::new(),
            ret_stack: Box::new(RetStack::new()),
            fast_hits: 0,
            interp_scratch: Vec::new(),
            pending_irq: None,
            retired: 0,
            sti_shadow: false,
        }
    }
}

/// Number of slots in the per-vcpu fast-resolve cache (R3). A power of two so the
/// index is a mask. 1024 entries × 16 bytes = 16 KiB per vcpu — cheap.
const FAST_BITS: u32 = 10;
const FAST_N: usize = 1 << FAST_BITS;

/// After this many IBTC refills a site is treated as megamorphic (R4): the
/// dispatcher stops refilling its slot (it stays empty → the site pays the
/// baseline dispatch forever) so a polymorphic indirect branch can't churn the
/// descriptor arena without bound.
const IBTC_MEGAMORPHIC_CAP: u32 = 8;

/// One direct-mapped fast-resolve entry: a guest RIP tag and its compiled entry.
/// A null `CompiledPtr` marks the slot empty (guest RIP 0 never collides with a
/// real block because an empty slot's pointer is null, checked first).
#[derive(Copy, Clone)]
struct FastEntry {
    rip: u64,
    entry: CompiledPtr,
}

/// Per-guest-thread execution context: CPU state + its own `run()` loop (§2).
pub struct Vcpu {
    pub cpu: CpuState,
    /// Decode/lift mode (§17.3), inherited from the `Vm`: threaded into every
    /// `resolve`/`lift`/`step_one` call and combined with the guest RIP to form the
    /// [`BlockKey`] a translation is cached under (§17.4). Constant for the vcpu's life
    /// (no runtime mode switching — the §17 scope fence).
    mode: CpuMode,
    /// Fast-resolve cache (fast-dispatch R3): a vcpu-private, direct-mapped RIP→compiled
    /// entry map that replaces the shared `RwLock<HashMap>` lookup (plus its two
    /// atomic counter bumps) for the transfers the chain loop can't chain — returns,
    /// indirect jumps, and cold outer-loop re-dispatch. Only `Compiled` entries are
    /// cached; interpreted blocks always route through `resolve` (so the cache
    /// counters stay meaningful). Flushed whenever the cache invalidation epoch
    /// moves — the coherence channel for cross-thread SMC that `invalidate_links`
    /// (backend-owned slots) cannot reach.
    fast: Box<[FastEntry; FAST_N]>,
    /// Snapshot of `TranslationCache::epoch` matching the current `fast` contents.
    fast_epoch: u64,
    /// Per-IBTC-slot refill count (R4), keyed by slot address. Guards against a
    /// megamorphic indirect site churning the descriptor arena: once a slot hits
    /// [`IBTC_MEGAMORPHIC_CAP`] refills the dispatcher stops filling it. Cleared on
    /// an invalidation-epoch change alongside the fast cache.
    ibtc_refills: HashMap<u64, u32>,
    /// Shadow return stack (fast-dispatch R5): compiled `call`s push predicted returns
    /// here, compiled `ret`s pop and chain on a match. Persists across `run()`
    /// calls (syscall exits re-enter `run()` constantly); its `sp` resets on an
    /// invalidation-epoch change. Boxed for a stable address and a small `Vcpu`.
    ret_stack: Box<RetStack>,
    /// Fast-resolve cache hits over this vcpu's lifetime (R6). A plain counter, not
    /// atomic — a shared atomic here would reintroduce exactly the contention R3
    /// removed. Read via [`Vcpu::fast_hits`].
    fast_hits: u64,
    /// Reused temps buffer for `interpret_block` — grows to the largest block's
    /// `temp_count` and is zero-filled per block, avoiding a per-dispatch allocation.
    interp_scratch: Vec<u64>,
    /// Pending maskable hardware-interrupt vector (§17.6, sub-seam c). Set by
    /// [`Vcpu::inject_irq`]; delivered at a `run()` boundary through the real-mode IVT
    /// once IF is set, no memory/port completion is outstanding, and the STI shadow has
    /// elapsed (see [`Vcpu::run`]). A plain `Vcpu` field, NOT in `#[repr(C)] CpuState`:
    /// it is embedder-facing async state, never read by compiled code, so it stays off
    /// the `jit_abi` layout entirely. Real-mode only (Long64/Compat32 never inject).
    /// Stays queued while blocked — never silently dropped.
    pending_irq: Option<u8>,
    /// Retired-instruction counter (§17.6, sub-seam c): a monotone `u64` bumped once per
    /// guest instruction that **retires** (completes) on the INTERPRETER path. This is
    /// the deterministic virtual-time base the embedder schedules against; it never reads
    /// a wall clock. Because Real16 is interpreter-only (the JIT/region tier is never
    /// reached in real mode) this counts every real-mode instruction. Compiled
    /// Long64/Compat32 blocks do NOT tick it — charging retirement inside compiled code
    /// would need codegen changes we deliberately avoid — so on the PS4 path it counts
    /// only the occasional interpreter single-step (MMIO retry). Read via
    /// [`Vcpu::retired_instructions`].
    retired: u64,
    /// STI-shadow latch (§17.6, sub-seam c). `sti` masks interrupt delivery for the
    /// duration of the *following* instruction, so `sti; hlt` and `sti; cli` behave
    /// atomically on real hardware. Set true when the just-run interpreted block ended
    /// with `sti` as its final retired instruction (the interpreter reports this); while
    /// set, a pending IRQ is held for one more block-dispatch boundary. Cleared once the
    /// next block runs an instruction (which clears the shadow). Delivery at a boundary is
    /// blocked while this is set. See [`Vcpu::run`].
    sti_shadow: bool,
}

impl Vcpu {
    /// Fast-resolve cache hits (R3) served without a shared-cache lookup (R6).
    pub fn fast_hits(&self) -> u64 {
        self.fast_hits
    }

    /// Queue a pending maskable **hardware interrupt** for real-mode delivery (§17.6,
    /// sub-seam c). `vector` is the IVT entry the embedder's PIC resolved (INTA);
    /// x86jit does NOT model a PIC/8259 — computing the vector and updating the PIC's
    /// in-service/ISR state on acknowledge is the **embedder's** responsibility, done
    /// before it calls this. The interrupt is delivered at the next [`Vcpu::run`]
    /// boundary (never mid-block) once ALL hold: IF is set, no memory/port completion is
    /// outstanding (`pending_mmio`/`pending_mmio_write`/`pending_port_in`), and the STI
    /// shadow has elapsed. If injection is currently blocked the vector stays queued —
    /// it is never silently dropped — and fires at the first boundary the gates open,
    /// including the boundary after a `hlt` wakeup. Only one interrupt is queued at a
    /// time (a real PIC re-asserts INTR for further lines after the embedder EOIs, so a
    /// later `inject_irq` overwrites a still-pending vector). Real-mode only.
    pub fn inject_irq(&mut self, vector: u8) {
        self.pending_irq = Some(vector);
    }

    /// Whether an injected hardware-interrupt vector is currently queued and not yet
    /// delivered (§17.6, sub-seam c) — e.g. still masked by IF, held by the STI shadow,
    /// or waiting on a memory/port completion. The embedder can consult this before
    /// re-injecting; a fresh [`Vcpu::inject_irq`] overwrites a still-queued vector.
    pub fn has_pending_irq(&self) -> bool {
        self.pending_irq.is_some()
    }

    /// The retired-instruction counter (§17.6, sub-seam c): guest instructions that have
    /// **retired** (completed) on the interpreter path since this vcpu was created. This
    /// is a deterministic, wall-clock-free virtual-time base for the embedder's scheduler.
    /// In real mode (interpreter-only) it counts every executed instruction; compiled
    /// Long64/Compat32 blocks do not tick it (see the field docs). Monotone, never reset.
    pub fn retired_instructions(&self) -> u64 {
        self.retired
    }

    /// Direct-mapped index for `rip` (Fibonacci hash — one multiply, good spread
    /// even for densely-packed short blocks).
    #[inline]
    fn fast_index(rip: u64) -> usize {
        (rip.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - FAST_BITS)) as usize
    }

    /// Probe the fast-resolve cache; `Some(entry)` on a hit.
    #[inline]
    fn fast_get(&self, rip: u64) -> Option<CompiledPtr> {
        let e = self.fast[Self::fast_index(rip)];
        if !e.entry.0.is_null() && e.rip == rip {
            Some(e.entry)
        } else {
            None
        }
    }

    /// Install a compiled entry for `rip` (overwrites any collision — a stale tag
    /// can only cost a miss, never a wrong transfer, and within an epoch a RIP maps
    /// to a stable compiled entry).
    #[inline]
    fn fast_put(&mut self, rip: u64, entry: CompiledPtr) {
        self.fast[Self::fast_index(rip)] = FastEntry { rip, entry };
    }

    /// Drop every fast-resolve entry and reset the IBTC refill counts (on an
    /// invalidation-epoch change). The backend already zeroed the slots
    /// (`invalidate_links`), so a previously-capped site gets a fresh chance to
    /// re-cache against the rewritten code.
    fn fast_clear(&mut self) {
        for e in self.fast.iter_mut() {
            e.entry = CompiledPtr(std::ptr::null());
        }
        self.ibtc_refills.clear();
        // Drop return predictions too (R5). Not required for correctness — the
        // `ret` addr-compare already guards every prediction — but it keeps the ring
        // from carrying frames whose continuation slots were just zeroed.
        self.ret_stack.sp = 0;
    }
}

impl Vcpu {
    /// Set a guest register. GPRs route through the central index map (§3.1);
    /// RIP and the FS/GS bases live in their own `CpuState` fields (§4.3).
    /// Size-dependent GPR write semantics (32-bit zeroing) are an M1 concern in
    /// the lift's write path — this API sets the full 64-bit value.
    pub fn set_reg(&mut self, reg: Reg, val: u64) {
        match reg.gpr_index() {
            Some(i) => self.cpu.gpr[i] = val,
            None => match reg {
                Reg::Rip => self.cpu.rip = val,
                Reg::FsBase => self.cpu.fs_base = val,
                Reg::GsBase => self.cpu.gs_base = val,
                // Real-mode segment selectors (§17.6): the embedder seeds these before a
                // Real16 run; the base is `selector << 4`.
                Reg::Cs => self.cpu.cs = val as u16,
                Reg::Ds => self.cpu.ds = val as u16,
                Reg::Es => self.cpu.es = val as u16,
                Reg::Ss => self.cpu.ss = val as u16,
                _ => unreachable!("gpr_index() only returns None for Rip/FsBase/GsBase/segments"),
            },
        }
    }

    /// Read a guest register. Mirror of [`Self::set_reg`]. (§4.3)
    pub fn reg(&self, reg: Reg) -> u64 {
        match reg.gpr_index() {
            Some(i) => self.cpu.gpr[i],
            None => match reg {
                Reg::Rip => self.cpu.rip,
                Reg::FsBase => self.cpu.fs_base,
                Reg::GsBase => self.cpu.gs_base,
                Reg::Cs => self.cpu.cs as u64,
                Reg::Ds => self.cpu.ds as u64,
                Reg::Es => self.cpu.es as u64,
                Reg::Ss => self.cpu.ss as u64,
                _ => unreachable!("gpr_index() only returns None for Rip/FsBase/GsBase/segments"),
            },
        }
    }

    pub fn set_flags(&mut self, flags: Flags) {
        self.cpu.flags = flags;
    }

    pub fn flags(&self) -> Flags {
        self.cpu.flags
    }

    pub fn set_xmm(&mut self, index: usize, value: u128) {
        self.cpu.xmm[index] = value;
    }

    pub fn xmm(&self, index: usize) -> u128 {
        self.cpu.xmm[index]
    }

    /// Upper 128 bits of YMM `index` (task-168.2).
    pub fn set_ymm_hi(&mut self, index: usize, value: u128) {
        self.cpu.ymm_hi[index] = value;
    }

    pub fn ymm_hi(&self, index: usize) -> u128 {
        self.cpu.ymm_hi[index]
    }

    /// Bits 511:256 of ZMM `index`: `half` 0 = 383:256, 1 = 511:384 (task-168.5).
    pub fn set_zmm_hi(&mut self, index: usize, half: usize, value: u128) {
        self.cpu.zmm_hi[index][half] = value;
    }

    pub fn zmm_hi(&self, index: usize, half: usize) -> u128 {
        self.cpu.zmm_hi[index][half]
    }

    /// Opmask register k`index` (k0–k7) (task-168.5).
    pub fn set_kmask(&mut self, index: usize, value: u64) {
        self.cpu.kmask[index] = value;
    }

    pub fn kmask(&self, index: usize) -> u64 {
        self.cpu.kmask[index]
    }

    /// Raw 10-byte 80-bit value of the PHYSICAL x87 register `index` (0..8), i.e.
    /// `fpr[index]` (task-188). `ST(i)` is `fpr[(fpu_top() + i) & 7]`; callers that
    /// want architectural order rotate by [`Self::fpu_top`].
    pub fn fpr_bytes(&self, index: usize) -> [u8; 10] {
        self.cpu.fpr[index]
    }

    /// Set the physical x87 register `index` from a raw 10-byte 80-bit value (task-188).
    pub fn set_fpr_bytes(&mut self, index: usize, bytes: &[u8; 10]) {
        self.cpu.fpr[index] = *bytes;
    }

    /// The x87 stack-top pointer: the physical register that is `ST(0)` (task-188).
    pub fn fpu_top(&self) -> u32 {
        self.cpu.fpu_top
    }

    pub fn set_fpu_top(&mut self, top: u32) {
        self.cpu.fpu_top = top & 7;
    }

    /// The x87 control word (round-trips `fldcw`/`fnstcw`) (task-188).
    pub fn fpu_cw(&self) -> u16 {
        self.cpu.fpu_cw
    }

    pub fn set_fpu_cw(&mut self, cw: u16) {
        self.cpu.fpu_cw = cw;
    }

    /// Deliver an MMIO read result after `Exit::MmioRead`, then resume (§5.2). The
    /// block re-executes from the faulting instruction (RIP was left there), and its
    /// first load consumes this value instead of re-trapping. Stored on `CpuState`,
    /// not a temp (temps die when the block returns). Interpreter path today —
    /// JIT-side MMIO trap/resume is deferred (M4-T10), and the JIT never emits an
    /// `Exit::MmioRead` for an inlined load, so this is only reached under the interp.
    pub fn complete_mmio_read(&mut self, value: u64) {
        self.cpu.pending_mmio = Some(value);
    }

    /// Acknowledge an `Exit::MmioWrite` after performing its side effect, then
    /// resume (§5.2). Symmetric to [`Self::complete_mmio_read`]: RIP was left on the
    /// faulting store, so the block re-executes from it; this flag tells the retried
    /// store the write is already done — skip it (no re-trap) and continue. Because
    /// the instruction re-runs, its non-store effects (RSP for `push`, flags) commit
    /// exactly once, preserving instruction atomicity (§7 pitfall 0). Interpreter
    /// path today; JIT-side MMIO is deferred (M4-T10).
    ///
    /// A read-modify-write to a `Trap` region (`add [mmio], reg`) is not supported:
    /// on retry its load re-traps as a fresh `MmioRead`. Model such a device with a
    /// pure load then a pure store instead.
    pub fn complete_mmio_write(&mut self) {
        self.cpu.pending_mmio_write = true;
    }

    /// Deliver the value read from a port after `Exit::PortIo { dir: In, .. }`, then
    /// resume (§5.2). Unlike MMIO, RIP already advanced past the `in` (as for a
    /// syscall), so this only writes the accumulator — the next `run()` continues
    /// from the following instruction. The write honours x86 sub-register semantics
    /// (32-bit zeroes the upper 32; 16/8-bit merge), reusing the central GPR write
    /// path. Only the low `size` bytes of `value` are used. Calling this without an
    /// outstanding `in` (e.g. after an `out`) is a no-op.
    pub fn complete_port_in(&mut self, value: u64) {
        if let Some(size) = self.cpu.pending_port_in.take() {
            self.cpu
                .write_gpr(Reg::Rax.gpr_index().unwrap(), value, size);
        }
    }

    /// Execute until an exit event or budget exhaustion (§5.1, §9.2).
    /// `budget` is measured in blocks (§5.1 recommendation).
    ///
    /// Compiled blocks are chained (§12 M5): a direct edge whose link slot is
    /// filled hands the next entry back via `MemCtx.next_entry` and the inner loop
    /// jumps straight there, skipping the cache lookup. The budget still ticks per
    /// block, so a tight chained loop yields `BudgetExhausted` (preemption, §9.2).
    pub fn run(&mut self, vm: &Vm, budget: Option<u64>) -> Exit {
        let mut blocks_run: u64 = 0;
        let mut ctx = MemCtx::for_memory(&vm.mem);
        // Hand compiled code this vcpu's shadow return stack (R5). `self.ret_stack`
        // is boxed, so its address is stable for the whole run despite `&mut self`.
        ctx.ret_stack = std::ptr::addr_of_mut!(*self.ret_stack) as u64;

        loop {
            if budget.is_some_and(|b| blocks_run >= b) {
                return Exit::BudgetExhausted;
            }

            // SMC (§10): drop any cached block a prior block's store landed on,
            // before fetching the next one, so re-execution re-lifts fresh bytes.
            vm.handle_smc();

            // Flush the fast-resolve cache if any invalidation happened since we
            // last synced (R3) — covers same-thread SMC (just handled above) and
            // cross-thread SMC (another vcpu bumped the epoch). Ordered after
            // `handle_smc` so a probe never predates its own invalidation.
            let epoch = vm.cache.epoch();
            if epoch != self.fast_epoch {
                self.fast_clear();
                self.fast_epoch = epoch;
            }

            // Resume after an MMIO trap the JIT deferred (§5.2, M4-T10): once the
            // embedder has supplied the read value (`complete_mmio_read`) or
            // acknowledged the write (`complete_mmio_write`), single-step the
            // faulting instruction on the interpreter — it consumes the pending
            // value/ack and advances RIP — before dispatching the next block. (Under
            // the interpreter backend this is equivalent to re-dispatching the block;
            // it just does the one faulting instruction first.)
            if self.cpu.pending_mmio.is_some() || self.cpu.pending_mmio_write {
                let mut info = crate::interp::RetireInfo::default();
                let r = crate::interp::step_one(
                    &vm.mem,
                    &mut self.cpu,
                    self.mode,
                    &mut self.interp_scratch,
                    &mut info,
                );
                self.retired += info.retired;
                self.sti_shadow = info.sti_shadow;
                match r {
                    StepResult::Continue => {
                        blocks_run += 1;
                        continue;
                    }
                    StepResult::Exit(exit) => return exit,
                }
            }

            // §17.6 (sub-seam c): hardware-interrupt injection is delivered here, at a
            // run() boundary — never mid-block. Gate: a vector is pending, IF is set, no
            // memory/port completion is outstanding (a partially-serviced MMIO/`in` must
            // finish first), and the one-instruction STI shadow has elapsed. All false-
            // gating leaves the vector queued (never dropped) for a later boundary — this
            // is also the point that services a post-`hlt` wakeup: after an `Exit::Hlt`
            // the embedder injects and re-enters, and RIP already sits past the `hlt`, so
            // delivery vectors the handler and `iret` resumes past the `hlt`. The
            // embedder owns the PIC: it must have run INTA / updated the ISR before
            // `inject_irq`. Real-mode only (Long64/Compat32 never set `pending_irq`).
            if let Some(vector) = self.pending_irq {
                let deliverable = self.cpu.flags.if_
                    && self.cpu.pending_mmio.is_none()
                    && !self.cpu.pending_mmio_write
                    && self.cpu.pending_port_in.is_none()
                    && !self.sti_shadow;
                if deliverable {
                    self.pending_irq = None;
                    // Saved IP = current RIP (the maskable IRQ is an async trap; execution
                    // resumes at the next instruction, which is where RIP already points).
                    let saved = self.cpu.rip;
                    match crate::interp::deliver_interrupt(
                        &mut self.cpu,
                        &vm.mem,
                        saved,
                        vector,
                        saved,
                    ) {
                        StepResult::Continue => {
                            // Delivery is itself an atomic hardware event, not a retired
                            // guest instruction — do not tick `retired`; do charge a block
                            // for §9.2 budget accounting. It also clears any stale shadow.
                            self.sti_shadow = false;
                            blocks_run += 1;
                            continue;
                        }
                        StepResult::Exit(exit) => return exit,
                    }
                }
            }

            // §17.6: the physical fetch address (`cs_base + IP` in Real16, else `rip`).
            // The block cache and lift key on `fetch.pa`; the fast-probe stays keyed on
            // the raw `rip` (it only ever holds *compiled* blocks, which Real16 never
            // produces — so an IP-keyed probe is harmless and identical outside Real16).
            let fetch = FetchAddr::for_mode(self.mode, self.cpu.rip, self.cpu.cs);

            // Fast path (R3): a vcpu-private probe replaces the shared cache lookup
            // for compiled blocks. A miss falls back to `resolve` and installs the
            // result; interpreted blocks are never cached here, so they always route
            // through `resolve` (keeping the cache hit/miss counters meaningful).
            let block = match self.fast_get(self.cpu.rip) {
                Some(entry) => {
                    // Plain (non-atomic) per-vcpu counter — the whole point of R3 is
                    // to avoid the shared atomic bumps on this path (R6 observability).
                    self.fast_hits += 1;
                    CachedBlock::Compiled { entry }
                }
                None => match resolve(vm, fetch, self.mode) {
                    Ok(b) => {
                        if let CachedBlock::Compiled { entry, .. } = &b {
                            self.fast_put(self.cpu.rip, *entry);
                        }
                        b
                    }
                    Err(exit) => return exit,
                },
            };

            match block {
                CachedBlock::Interpreted(ir) => {
                    let mut info = crate::interp::RetireInfo::default();
                    let r = crate::interp::interpret_block(
                        &ir,
                        &mut self.cpu,
                        &vm.mem,
                        &mut self.interp_scratch,
                        &mut info,
                    );
                    // §17.6 (sub-seam c): tick the retired-instruction counter and latch
                    // the STI shadow from this interpreted block (Real16 is all
                    // interpreter, so this is the whole real-mode instruction stream).
                    self.retired += info.retired;
                    self.sti_shadow = info.sti_shadow;
                    match r {
                        StepResult::Continue => blocks_run += 1,
                        // §17.6 (sub-seam b): in real mode a CPU exception is delivered
                        // in-guest through the IVT, not surfaced to the embedder. The
                        // only `Exit::Exception` that escapes an interpreted Real16 block
                        // is `#DE` (divide error, vector 0) from `IrOp::Div` — `int n`/
                        // `int3`/`ud2` already vector in-guest via `IrOp::IntGate`. Its
                        // `addr` is the faulting instruction's IP (the saved IP for a
                        // fault), so re-deliver through the same IVT path and continue.
                        // Long64/Compat32 still return `Exit::Exception` unchanged.
                        StepResult::Exit(Exit::Exception { addr, vector })
                            if self.mode == CpuMode::Real16 =>
                        {
                            match crate::interp::deliver_interrupt(
                                &mut self.cpu,
                                &vm.mem,
                                addr,
                                vector,
                                addr,
                            ) {
                                StepResult::Continue => blocks_run += 1,
                                StepResult::Exit(exit) => return exit,
                            }
                        }
                        StepResult::Exit(exit) => return exit,
                    }
                }
                CachedBlock::Compiled { entry, .. } => {
                    let mut cur = entry;
                    loop {
                        // Hand the block its remaining block quantum (superblocks
                        // M5-T3): a compiled region spends 1 fuel per guest block and
                        // stops at 0; a single block ignores it. Charging the fuel it
                        // consumed (min 1) keeps `blocks_run` an exact guest-block
                        // count — identical to the interpreter, preserving §9.2 and
                        // the `RunSpec::Blocks(n)` oracle.
                        let quantum = budget.map_or(u64::MAX, |b| b - blocks_run);
                        ctx.fuel = quantum;
                        // SAFETY: `cur` is a block compiled to this ABI, alive in
                        // the JIT arena (owned by `vm`) for the call.
                        let code = unsafe { call_block(cur, &mut self.cpu, &mut ctx) };
                        blocks_run += (quantum - ctx.fuel).max(1);
                        match code {
                            RET_CONTINUE => break,
                            RET_CHAIN => {
                                vm.cache.record_chain();
                                cur = CompiledPtr(ctx.next_entry as *const u8);
                            }
                            RET_LINK => match resolve(vm, FetchAddr::flat(self.cpu.rip), self.mode)
                            {
                                Ok(CachedBlock::Compiled { entry, .. }) => {
                                    // SAFETY: `link_slot` is a live `Box<AtomicU64>`
                                    // in the JIT arena. Relaxed store: another vcpu
                                    // reading the slot sees 0 or a valid entry, never a
                                    // torn value (aligned u64); it pairs with the
                                    // backend's `invalidate_links` clear (R1, M7).
                                    unsafe {
                                        (*(ctx.link_slot as *const AtomicU64))
                                            .store(entry.0 as u64, Ordering::Relaxed)
                                    };
                                    // Seed the fast-resolve cache too (R3): the next
                                    // outer-loop visit to this RIP skips `resolve`.
                                    self.fast_put(self.cpu.rip, entry);
                                    cur = entry;
                                }
                                // Mixed backend can't chain — fall back to dispatch.
                                Ok(CachedBlock::Interpreted(_)) => break,
                                Err(exit) => return exit,
                            },
                            // IBTC miss (R4): an indirect edge whose per-site slot was
                            // empty or held a different target. Resolve the computed
                            // RIP and refill the slot with a fresh {target, entry}
                            // descriptor, unless the site is megamorphic.
                            RET_IBTC_MISS => {
                                match resolve(vm, FetchAddr::flat(self.cpu.rip), self.mode) {
                                    Ok(CachedBlock::Compiled { entry, .. }) => {
                                        let slot = ctx.link_slot;
                                        let count = self.ibtc_refills.entry(slot).or_insert(0);
                                        if *count < IBTC_MEGAMORPHIC_CAP {
                                            *count += 1;
                                            let desc =
                                                vm.cache.alloc_ibtc_descriptor(self.cpu.rip, entry);
                                            // SAFETY: `slot` is a live `Box<AtomicU64>` in
                                            // the JIT arena; the published descriptor is
                                            // immutable and never freed (R4 coherence).
                                            // Release (not Relaxed): unlike the RET_LINK
                                            // slot — a single scalar entry — this publishes
                                            // a POINTER to a multi-field {target, entry}
                                            // payload, so the payload's writes must be
                                            // ordered-visible before the pointer. Release
                                            // here pairs with the reader's address
                                            // dependency (the compiled `ibtc_or_miss` loads
                                            // the descriptor fields *through* this pointer),
                                            // giving release/consume ordering; a plain
                                            // Relaxed store would let a weakly-ordered host
                                            // (AArch64) expose the pointer before the fields.
                                            unsafe {
                                                (*(slot as *const AtomicU64))
                                                    .store(desc, Ordering::Release)
                                            };
                                        }
                                        self.fast_put(self.cpu.rip, entry);
                                        cur = entry;
                                    }
                                    // Indirect edge into an interpreted block — dispatch.
                                    Ok(CachedBlock::Interpreted(_)) => break,
                                    Err(exit) => return exit,
                                }
                            }
                            RET_SYSCALL => return Exit::Syscall,
                            RET_HLT => return Exit::Hlt,
                            RET_UNMAPPED => return ctx.unmapped_exit(),
                            // Inlined access to a Trap region (M4-T10): single-step
                            // the faulting instruction on the interpreter, which
                            // produces the MmioRead/Write exit (nothing committed).
                            RET_MMIO_DEFER => {
                                // Interpreter single-step (not the hot compiled loop):
                                // tick the retired counter for the one instruction it
                                // may retire (§17.6, sub-seam c).
                                let mut info = crate::interp::RetireInfo::default();
                                let r = crate::interp::step_one(
                                    &vm.mem,
                                    &mut self.cpu,
                                    self.mode,
                                    &mut self.interp_scratch,
                                    &mut info,
                                );
                                self.retired += info.retired;
                                match r {
                                    StepResult::Continue => break,
                                    StepResult::Exit(exit) => return exit,
                                }
                            }
                            // A port-I/O instruction (`in`/`out`, §5.2): the block set
                            // RIP to the instruction; single-step it on the interpreter
                            // to produce the `Exit::PortIo` (same deferral as MMIO).
                            RET_PORTIO_DEFER => {
                                let mut info = crate::interp::RetireInfo::default();
                                let r = crate::interp::step_one(
                                    &vm.mem,
                                    &mut self.cpu,
                                    self.mode,
                                    &mut self.interp_scratch,
                                    &mut info,
                                );
                                self.retired += info.retired;
                                match r {
                                    StepResult::Continue => break,
                                    StepResult::Exit(exit) => return exit,
                                }
                            }
                            // A guest exception (`#DE` div, or a lifted `ud2`/`int3`/
                            // `int1` trap); the block set the saved RIP (fault: on the
                            // instruction, trap: past it) and stored the vector in the
                            // MemCtx out-field.
                            RET_EXCEPTION => {
                                return Exit::Exception {
                                    addr: self.cpu.rip,
                                    vector: ctx.exception_vector as u8,
                                }
                            }
                            other => panic!("compiled block returned invalid ABI code {other}"),
                        }
                        if budget.is_some_and(|b| blocks_run >= b) {
                            return Exit::BudgetExhausted;
                        }
                    }
                }
            }
        }
    }
}

/// Fetch a block from the cache or lift+materialize it (miss). Lift errors are
/// legal exits (not `run()` failures) telling the user what to add (§9.2).
/// Publish every completed background compile into the cache (bg-tier, doc-27 D2 /
/// decision-5: the dispatcher publishes, the backend never touches the cache). Each
/// is epoch-checked by `upgrade` (a stale compile whose block was SMC-dropped is
/// rejected); the in-flight marker is always cleared so a rejected block can be
/// re-lifted and re-submitted. `tier_up_finished` short-circuits when idle.
fn drain_tier_up(vm: &Vm) {
    for fin in vm.backend.tier_up_finished() {
        // A Vm runs in a single mode (§17 scope fence), so the finished compile's
        // block key is its echoed pc under the Vm's mode (§17.4).
        let key = BlockKey::new(fin.pc, vm.mode);
        // Multi-span publish (BGT-6): a region carries one span per sub-block; a block
        // carries one. `on_mark` re-tags the pages under the spans lock (#12).
        let multi_span = fin.spans.len() > 1;
        let published = vm
            .cache
            .upgrade_region(key, fin.block, fin.spans, fin.epoch, |sp| {
                for (start, len) in sp {
                    vm.mem.mark_code(*start, *len);
                }
            });
        vm.cache.end_tier_up(key);
        if published {
            vm.cache.record_tier_bg_published();
            // A multi-span unit is a superblock region (BGT-6) — count it like the
            // eager region path does, so `cache.regions()` reflects background regions.
            if multi_span {
                vm.cache.record_region();
            }
        } else {
            vm.cache.record_tier_bg_rejected();
        }
    }
}

fn resolve(vm: &Vm, at: FetchAddr, mode: CpuMode) -> Result<CachedBlock, Exit> {
    // §17.4: cache maps key on { physical-fetch-address, mode }. In Real16 the physical
    // fetch address (`cs_base + IP`) is what keys the cache and tags SMC pages, so
    // blocks never collide across segments; the IR's `guest_start` stays the decode IP
    // (`at.ip`) so fall-through / branch targets remain 16-bit IPs (§17.6). Outside
    // Real16 `at.pa == at.ip == pc`, so this is byte-identical to the old `pc` keying.
    let pc = at.pa;
    let key = BlockKey::new(pc, mode);
    loop {
        // bg-tier (doc-27 D2): publish any completed background compiles first, so a
        // freshly-landed unit is seen by the lookup below. Cheap when idle (the
        // backend's ready-probe short-circuits an empty drain).
        if vm.tier_up_background {
            drain_tier_up(vm);
        }
        // Snapshot the invalidation epoch BEFORE the lookup: `upgrade` rejects the
        // tier-up if an SMC drop moved it in between. Otherwise a tier-up racing a
        // concurrent `invalidate_overlapping` would resurrect a stale block with no
        // span — permanently uninvalidatable (#3).
        let epoch = vm.cache.epoch();
        let Some(block) = vm.cache.get(key) else {
            break;
        };
        // Hotness-gated tier-up (FD tiering): a cached *interpreted* block that has
        // now run `tier_up_after` times gets JIT-compiled from its already-lifted
        // IR and swapped in, so cold one-shot blocks never pay compile cost while
        // hot blocks still tier up.
        let (Some(thr), CachedBlock::Interpreted(ir)) = (vm.tier_up_after, &block) else {
            return Ok(block);
        };
        let count = vm.cache.bump_hotness(key);
        if count < thr {
            return Ok(block);
        }
        // Background tier-up (doc-27 D4): compile off the vcpu and keep interpreting
        // until the result lands (published by `drain_tier_up` above on a later
        // dispatch). Submit once — `try_begin_tier_up` gates re-submission.
        if vm.tier_up_background {
            // Adaptive per-block tier (task-156). A region-forming backend decides once,
            // per pc, whether this is a multi-block loop worth a region. A loop stays
            // interpreted until a much higher backedge threshold T2 (a premature region
            // on a short loop regresses, T3f) — the OSR analogue; a non-loop block (or a
            // `None`-caps backend) tiers the single block at T1 as before.
            let region_candidate = match vm.backend.region_caps() {
                Some(caps) => match vm.cache.region_decision(key) {
                    Some(c) => c,
                    None => {
                        let c = matches!(
                            lift_region(&vm.mem, pc, caps, mode),
                            Ok(r) if r.blocks.len() > 1 && r.has_loop
                        );
                        vm.cache.set_region_decision(key, c);
                        c
                    }
                },
                None => false,
            };
            let (unit, spans) = if region_candidate {
                let t2 = vm.tier_up_region_after.unwrap_or(thr);
                if count < t2 {
                    return Ok(block); // a hot loop, still warming toward the region tier
                }
                if !vm.cache.try_begin_tier_up(key) {
                    return Ok(block); // a compile for this pc is already in flight
                }
                let caps = vm
                    .backend
                    .region_caps()
                    .expect("region candidate ⇒ region_caps");
                match lift_region(&vm.mem, pc, caps, mode) {
                    Ok(region) if region.blocks.len() > 1 && region.has_loop => {
                        let spans = region.spans();
                        (TierUpUnit::Region(Arc::new(region)), spans)
                    }
                    // SMC turned it non-loop since the decision — tier the single block.
                    _ => (
                        TierUpUnit::Block(ir.clone()),
                        vec![(ir.guest_start, ir.guest_len)],
                    ),
                }
            } else {
                if !vm.cache.try_begin_tier_up(key) {
                    return Ok(block); // a compile for this pc is already in flight
                }
                (
                    TierUpUnit::Block(ir.clone()),
                    vec![(ir.guest_start, ir.guest_len)],
                )
            };
            let req = TierUpRequest {
                pc,
                unit,
                consistency: vm.consistency,
                mmio: vm.mem.trap_window(),
                guest_base: vm.mem.guest_base(),
                spans,
                epoch,
            };
            match vm.backend.tier_up_async(req) {
                // Off to the worker — this block stays interpreted for now.
                TierUpSubmit::Queued => return Ok(block),
                // Queue full: don't compile inline (that reintroduces the spike);
                // drop the marker so hotness re-submits on a later dispatch.
                TierUpSubmit::Busy => {
                    vm.cache.end_tier_up(key);
                    return Ok(block);
                }
                // No worker (interpreter, or the JIT with bg off): fall through to
                // today's inline tier-up.
                TierUpSubmit::Unsupported => vm.cache.end_tier_up(key),
            }
        }
        let compiled = vm.materialize(ir);
        if vm
            .cache
            .upgrade(key, compiled.clone(), (ir.guest_start, ir.guest_len), epoch)
        {
            return Ok(compiled);
        }
        // Lost the race: an SMC drop invalidated the block mid-tier-up. Loop to
        // re-fetch / re-lift from current memory rather than run a stale block.
    }
    // Region path (§12 M5-T3): a region-forming backend lifts a superblock EAGERLY on
    // first sight. BGT-6 (doc-27 Phase 6): when background tier-up is on, skip this —
    // regions then form only for proven-hot loops, off-thread (in the hotness path
    // above), never the heavy inline compile T3f flagged. A multi-block region compiles
    // as one unit spanning all its sub-blocks; a one-block region falls through to the
    // single-block path (reusing the block already lifted, so no double lift).
    if let Some(caps) = vm.backend.region_caps().filter(|_| !vm.tier_up_background) {
        match lift_region(&vm.mem, pc, caps, mode) {
            // NOTE: the region path is JIT-only (a region-forming backend); Real16 stays
            // on the interpreter and never reaches here, so `pc` (flat) is correct.
            // Only a multi-block region *with a loop* is worth its heavier compile
            // (it amortizes over the iterations); everything else stays single-block.
            Ok(region) if region.blocks.len() > 1 && region.has_loop => {
                let spans = region.spans();
                let materialized = vm.backend.materialize_region(
                    &region,
                    vm.consistency,
                    vm.mem.trap_window(),
                    vm.mem.guest_base(),
                );
                // §10: tag every sub-block's pages — under the spans lock (#12).
                vm.cache.insert(key, materialized.clone(), spans, |sp| {
                    for (start, len) in sp {
                        vm.mem.mark_code(*start, *len);
                    }
                });
                vm.cache.record_region();
                return Ok(materialized);
            }
            Ok(region) => {
                return Ok(finish_single(
                    vm,
                    key,
                    pc,
                    region.blocks.into_iter().next().unwrap(),
                ))
            }
            Err(e) => return Err(lift_exit(e)),
        }
    }
    match lift_block(&vm.mem, at, mode) {
        Ok(ir) => Ok(finish_single(vm, key, pc, ir)),
        Err(e) => Err(lift_exit(e)),
    }
}

/// Materialize a single block, cache it under its `key` with its one span, and tag
/// its pages. `span_start` is the physical fetch address (== `ir.guest_start` outside
/// Real16, but the `cs_base + IP` physical address in Real16, where SMC page-tagging
/// and the cache key must both be physical — see `resolve`).
fn finish_single(vm: &Vm, key: BlockKey, span_start: u64, ir: IrBlock) -> CachedBlock {
    let (start, len) = (span_start, ir.guest_len);
    // FD tiering: defer compilation — a fresh block starts interpreted and is only
    // JIT-compiled once it proves hot (see `resolve`). Eager (tier_up_after None)
    // compiles immediately, the original behavior.
    let materialized = if vm.tier_up_after.is_some() {
        CachedBlock::Interpreted(Arc::new(ir))
    } else {
        vm.materialize(&ir)
    };
    // §10: tag the block's pages under the spans lock, so the tag can't be cleared
    // by a concurrent SMC invalidation between insert and mark (#12).
    vm.cache
        .insert(key, materialized.clone(), vec![(start, len)], |_| {
            vm.mem.mark_code(start, len)
        });
    materialized
}

/// Map a lift error to its dispatcher exit (a legal exit, not a `run()` failure).
fn lift_exit(e: LiftError) -> Exit {
    match e {
        LiftError::Unsupported { addr, bytes, len } => {
            Exit::UnknownInstruction { addr, bytes, len }
        }
        LiftError::DecodeFault { addr } => Exit::UnmappedMemory {
            addr,
            access: AccessKind::Execute,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vcpu() -> Vcpu {
        let vm = Vm::new(VmConfig {
            memory_model: MemoryModel::Flat { size: 0x1000 },
            consistency: MemConsistency::Fast,
        });
        vm.new_vcpu()
    }

    /// A fresh Vm defaults to long mode; vcpus inherit it (§17.3).
    #[test]
    fn vm_defaults_to_long_mode() {
        let vm = Vm::new(VmConfig {
            memory_model: MemoryModel::Flat { size: 0x1000 },
            consistency: MemConsistency::Fast,
        });
        assert_eq!(vm.cpu_mode(), CpuMode::Long64);
        assert_eq!(vm.new_vcpu().mode, CpuMode::Long64);
    }

    #[test]
    fn set_cpu_mode_accepts_compat32() {
        let mut vm = Vm::new(VmConfig {
            memory_model: MemoryModel::Flat { size: 0x1000 },
            consistency: MemConsistency::Fast,
        });
        vm.set_cpu_mode(CpuMode::Compat32);
        assert_eq!(vm.cpu_mode(), CpuMode::Compat32);
        assert_eq!(vm.new_vcpu().mode, CpuMode::Compat32);
    }

    #[test]
    fn fork_gives_the_child_independent_memory() {
        let mut vm = Vm::new(VmConfig {
            memory_model: MemoryModel::Flat { size: 0x2000 },
            consistency: MemConsistency::Fast,
        });
        vm.map(0, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
        vm.mem.write_bytes(0x100, &[1, 2, 3, 4]).unwrap();

        // Child inherits the snapshot...
        let child = vm
            .fork_with_backend(Box::new(InterpreterBackend))
            .expect("Flat memory is deep-copyable");
        let mut buf = [0u8; 4];
        child.mem.read_bytes(0x100, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4], "child sees the forked contents");

        // ...but writes don't cross over.
        child.mem.write_bytes(0x100, &[9, 9, 9, 9]).unwrap();
        let mut pbuf = [0u8; 4];
        vm.mem.read_bytes(0x100, &mut pbuf).unwrap();
        assert_eq!(pbuf, [1, 2, 3, 4], "parent memory unchanged by child write");
        let mut cbuf = [0u8; 4];
        child.mem.read_bytes(0x100, &mut cbuf).unwrap();
        assert_eq!(cbuf, [9, 9, 9, 9], "child memory changed");
    }

    #[test]
    fn gpr_roundtrip_through_index_map() {
        let mut c = vcpu();
        c.set_reg(Reg::Rax, 0xAA);
        c.set_reg(Reg::Rbx, 0xBB);
        c.set_reg(Reg::Rsp, 0x5050);
        c.set_reg(Reg::R15, 0xF15);
        assert_eq!(c.reg(Reg::Rax), 0xAA);
        assert_eq!(c.reg(Reg::Rbx), 0xBB);
        assert_eq!(c.reg(Reg::Rsp), 0x5050);
        assert_eq!(c.reg(Reg::R15), 0xF15);
    }

    #[test]
    fn gpr_writes_land_at_encoding_order_indices() {
        let mut c = vcpu();
        c.set_reg(Reg::Rbx, 0xB); // encoding index 3, not enum position 1
        assert_eq!(c.cpu.gpr[3], 0xB);
        assert_eq!(c.cpu.gpr[1], 0); // Rcx's slot untouched
    }

    #[test]
    fn rip_and_segment_bases_use_own_fields() {
        let mut c = vcpu();
        c.set_reg(Reg::Rip, 0x400000);
        c.set_reg(Reg::FsBase, 0x7fff_0000);
        c.set_reg(Reg::GsBase, 0x7fff_1000);
        assert_eq!(c.reg(Reg::Rip), 0x400000);
        assert_eq!(c.reg(Reg::FsBase), 0x7fff_0000);
        assert_eq!(c.reg(Reg::GsBase), 0x7fff_1000);
        assert_eq!(c.cpu.rip, 0x400000);
        assert_eq!(c.cpu.fs_base, 0x7fff_0000);
        assert_eq!(c.cpu.gs_base, 0x7fff_1000);
    }
}
