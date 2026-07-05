//! `Vm` (shared) and `Vcpu` (per-thread) — the KVM-style split (§2), plus the
//! dispatcher loop (§9.2).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::cache::{CachedBlock, CompiledPtr, TranslationCache};
use crate::exit::{AccessKind, Exit, StepResult};
use crate::ir::{IrBlock, IrRegion, RegionCaps};
use crate::jit_abi::{
    call_block, MemCtx, RetStack, RET_CHAIN, RET_CONTINUE, RET_EXCEPTION, RET_HLT, RET_IBTC_MISS,
    RET_LINK, RET_SYSCALL, RET_UNMAPPED,
};
use crate::lift::{lift_block, lift_region, LiftError};
use crate::memory::{MapError, MemError, Memory, MemoryModel, Prot, RegionKind};
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
    fn materialize(&self, ir: &IrBlock, consistency: MemConsistency) -> CachedBlock;

    /// Superblock caps if this backend forms regions (§12 M5-T3), else `None`
    /// (the default). When `Some`, the dispatcher lifts a region and calls
    /// [`materialize_region`](Backend::materialize_region); a one-block region
    /// falls back to `materialize`.
    fn region_caps(&self) -> Option<RegionCaps> {
        None
    }

    /// Compile a multi-block region into one unit. Only called when
    /// [`region_caps`](Backend::region_caps) is `Some`; the default is unreachable.
    fn materialize_region(&self, _region: &IrRegion, _consistency: MemConsistency) -> CachedBlock {
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
}

/// Default backend: wrap the IR in an `Arc` and interpret it (§8.1).
pub struct InterpreterBackend;

impl Backend for InterpreterBackend {
    fn materialize(&self, ir: &IrBlock, _consistency: MemConsistency) -> CachedBlock {
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

    /// Fork this VM: a child with an independent deep-copy of guest memory (§4.2),
    /// a fresh translation cache, and the given backend, inheriting the consistency
    /// tier and tier-up policy. The guest-agnostic primitive behind an OS `fork` —
    /// the embedder clones the vcpu's `CpuState` (child RAX = 0) and drives the
    /// child. Knows nothing about processes, pids, or fds.
    pub fn fork_with_backend(&self, backend: Box<dyn Backend>) -> Vm {
        Vm {
            mem: self.mem.deep_copy(),
            cache: TranslationCache::new(),
            backend,
            consistency: self.consistency,
            tier_up_after: self.tier_up_after,
        }
    }

    /// Construct with an injected backend — this is how the JIT gets in (§4.1).
    pub fn with_backend(config: VmConfig, backend: Box<dyn Backend>) -> Self {
        Self {
            mem: Memory::new(config.memory_model),
            cache: TranslationCache::new(),
            backend,
            consistency: config.consistency,
            tier_up_after: None,
        }
    }

    pub fn map(
        &mut self,
        guest_addr: u64,
        size: usize,
        prot: Prot,
        kind: RegionKind,
    ) -> Result<(), MapError> {
        self.mem.map(guest_addr, size, prot, kind)
    }

    pub fn write_bytes(&mut self, guest_addr: u64, bytes: &[u8]) -> Result<(), MemError> {
        self.mem.write_bytes(guest_addr, bytes)
    }

    pub fn read_bytes(&self, guest_addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.mem.read_bytes(guest_addr, buf)
    }

    pub fn unmap(&mut self, guest_addr: u64, size: usize) -> Result<(), MapError> {
        self.mem.unmap(guest_addr, size)
    }

    /// Materialize a lifted block via the injected backend (§8).
    fn materialize(&self, ir: &IrBlock) -> CachedBlock {
        self.backend.materialize(ir, self.consistency)
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
        Vcpu {
            cpu: CpuState::new(),
            pending_mmio: None,
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
        }
    }
}

/// A value supplied by `complete_mmio_read`, waiting to be consumed by the
/// re-executed load at `addr` (§5.2). Not written into a temp (temps die on
/// block return) — matched by the retried `Load` in the memory layer.
#[derive(Copy, Clone, Debug)]
pub struct PendingMmio {
    pub addr: u64,
    pub size: u8,
    pub value: u64,
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
    /// Set by `complete_mmio_read`, consumed by the retried load (§5.2).
    pub pending_mmio: Option<PendingMmio>,
    // Breakpoints (Exit::Breakpoint) land here too once debug support exists (§14).
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
}

impl Vcpu {
    /// Fast-resolve cache hits (R3) served without a shared-cache lookup (R6).
    pub fn fast_hits(&self) -> u64 {
        self.fast_hits
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
                _ => unreachable!("gpr_index() only returns None for Rip/FsBase/GsBase"),
            },
        }
    }

    /// Read a guest register. Mirror of [`set_reg`]. (§4.3)
    pub fn reg(&self, reg: Reg) -> u64 {
        match reg.gpr_index() {
            Some(i) => self.cpu.gpr[i],
            None => match reg {
                Reg::Rip => self.cpu.rip,
                Reg::FsBase => self.cpu.fs_base,
                Reg::GsBase => self.cpu.gs_base,
                _ => unreachable!("gpr_index() only returns None for Rip/FsBase/GsBase"),
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

    /// Deliver an MMIO read result after `Exit::MmioRead`, then resume (§5.2).
    /// Stores `(addr, size, value)` as a PENDING value; the retried load (RIP is
    /// on the faulting instruction) consumes it instead of trapping. NOT a write
    /// into a temp — temps die when the block returns (works in interp AND JIT).
    pub fn complete_mmio_read(&mut self, value: u64) {
        // The MmioRead exit carried (addr, size); store them alongside `value` so
        // the retried Load can match and consume it. Wiring in M2.
        let _ = value;
        todo!("M2: set self.pending_mmio = Some(PendingMmio{{addr,size,value}}) (§5.2)")
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

            // Fast path (R3): a vcpu-private probe replaces the shared cache lookup
            // for compiled blocks. A miss falls back to `resolve` and installs the
            // result; interpreted blocks are never cached here, so they always route
            // through `resolve` (keeping the cache hit/miss counters meaningful).
            let block = match self.fast_get(self.cpu.rip) {
                Some(entry) => {
                    // Plain (non-atomic) per-vcpu counter — the whole point of R3 is
                    // to avoid the shared atomic bumps on this path (R6 observability).
                    self.fast_hits += 1;
                    CachedBlock::Compiled {
                        entry,
                        guest_len: 0,
                    }
                }
                None => match resolve(vm, self.cpu.rip) {
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
                    match crate::interp::interpret_block(&ir, &mut self.cpu, &vm.mem) {
                        StepResult::Continue => blocks_run += 1,
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
                            RET_LINK => match resolve(vm, self.cpu.rip) {
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
                            RET_IBTC_MISS => match resolve(vm, self.cpu.rip) {
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
                            },
                            RET_SYSCALL => return Exit::Syscall,
                            RET_HLT => return Exit::Hlt,
                            RET_UNMAPPED => return ctx.unmapped_exit(),
                            // Today only #DE (vector 0); RIP is on the faulting insn.
                            RET_EXCEPTION => {
                                return Exit::Exception {
                                    addr: self.cpu.rip,
                                    vector: 0,
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
fn resolve(vm: &Vm, pc: u64) -> Result<CachedBlock, Exit> {
    loop {
        // Snapshot the invalidation epoch BEFORE the lookup: `upgrade` rejects the
        // tier-up if an SMC drop moved it in between. Otherwise a tier-up racing a
        // concurrent `invalidate_overlapping` would resurrect a stale block with no
        // span — permanently uninvalidatable (#3).
        let epoch = vm.cache.epoch();
        let Some(block) = vm.cache.get(pc) else { break };
        // Hotness-gated tier-up (FD tiering): a cached *interpreted* block that has
        // now run `tier_up_after` times gets JIT-compiled from its already-lifted
        // IR and swapped in, so cold one-shot blocks never pay compile cost while
        // hot blocks still tier up.
        let (Some(thr), CachedBlock::Interpreted(ir)) = (vm.tier_up_after, &block) else {
            return Ok(block);
        };
        if vm.cache.bump_hotness(pc) < thr {
            return Ok(block);
        }
        let compiled = vm.materialize(ir);
        if vm
            .cache
            .upgrade(pc, compiled.clone(), (ir.guest_start, ir.guest_len), epoch)
        {
            return Ok(compiled);
        }
        // Lost the race: an SMC drop invalidated the block mid-tier-up. Loop to
        // re-fetch / re-lift from current memory rather than run a stale block.
    }
    // Region path (§12 M5-T3): a region-forming backend lifts a superblock. A
    // multi-block region compiles as one unit spanning all its sub-blocks; a
    // one-block region falls through to the single-block path (reusing the block
    // already lifted, so no double lift).
    if let Some(caps) = vm.backend.region_caps() {
        match lift_region(&vm.mem, pc, caps) {
            // Only a multi-block region *with a loop* is worth its heavier compile
            // (it amortizes over the iterations); everything else stays single-block.
            Ok(region) if region.blocks.len() > 1 && region.has_loop => {
                let spans = region.spans();
                let materialized = vm.backend.materialize_region(&region, vm.consistency);
                // §10: tag every sub-block's pages — under the spans lock (#12).
                vm.cache.insert(pc, materialized.clone(), spans, |sp| {
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
                    pc,
                    region.blocks.into_iter().next().unwrap(),
                ))
            }
            Err(e) => return Err(lift_exit(e)),
        }
    }
    match lift_block(&vm.mem, pc) {
        Ok(ir) => Ok(finish_single(vm, pc, ir)),
        Err(e) => Err(lift_exit(e)),
    }
}

/// Materialize a single block, cache it with its one span, and tag its pages.
fn finish_single(vm: &Vm, pc: u64, ir: IrBlock) -> CachedBlock {
    let (start, len) = (ir.guest_start, ir.guest_len);
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
    vm.cache.insert(pc, materialized.clone(), vec![(start, len)], |_| {
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

    #[test]
    fn fork_gives_the_child_independent_memory() {
        let mut vm = Vm::new(VmConfig {
            memory_model: MemoryModel::Flat { size: 0x2000 },
            consistency: MemConsistency::Fast,
        });
        vm.map(0, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
        vm.mem.write_bytes(0x100, &[1, 2, 3, 4]).unwrap();

        // Child inherits the snapshot...
        let mut child = vm.fork_with_backend(Box::new(InterpreterBackend));
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
