//! Translation cache keyed by [`BlockKey`] — guest address plus decode mode (§9.1, §17.4).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::ir::IrBlock;
use crate::lift::CpuMode;

/// Translation-cache key (§17.4): the same guest bytes decode and lift differently per
/// decode mode (mode × CS.D), so a translation is identified by *both* its entry address
/// and the [`CpuMode`] it was lifted under — never the address alone. A future protected
/// mode adds a `CpuMode` variant, not a new key shape; segment bases stay out of the key
/// (they are runtime `CpuState`, per the FS/GS pattern), so a segment reload never
/// invalidates a translation.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct BlockKey {
    pub guest_addr: u64,
    pub mode: CpuMode,
}

impl BlockKey {
    pub fn new(guest_addr: u64, mode: CpuMode) -> Self {
        Self { guest_addr, mode }
    }
}

/// A raw pointer into the JIT code arena.
///
/// Manually `Send + Sync`: compiled code is read-only and executable from any
/// thread; its lifetime is owned by the JIT arena, which lives as long as the
/// `Vm` (§9.1). Declaring this in M4 (not M7) avoids reworking signatures once
/// threads arrive — the M7 trap.
#[derive(Copy, Clone)]
pub struct CompiledPtr(pub *const u8);

// SAFETY: see the type-level comment.
unsafe impl Send for CompiledPtr {}
unsafe impl Sync for CompiledPtr {}

/// A materialized block. The backend produces it; the dispatcher executes it
/// uniformly by matching on the variant (§8).
#[derive(Clone)]
pub enum CachedBlock {
    Interpreted(Arc<IrBlock>),
    Compiled { entry: CompiledPtr },
}

/// Shared translation cache. Cloned-out on `get` so no lock guard is held
/// across block execution (which may mutate memory -> SMC invalidation) (§9.2).
pub struct TranslationCache {
    // §17.4: keyed by BlockKey { guest_addr, mode } — the same bytes lift differently
    // per decode mode, so translations never alias across modes.
    map: RwLock<HashMap<BlockKey, CachedBlock>>,
    // Guest byte spans of each cached unit, keyed by its `BlockKey`. A single
    // block has one `(start, len)`; a superblock (M5-T3) has one per sub-block,
    // possibly non-contiguous. A write overlapping ANY span drops the whole unit
    // (§10) — SMC is address-scoped, so `invalidate_overlapping` matches spans by
    // address across every mode. Kept in lockstep with `map`.
    spans: RwLock<HashMap<BlockKey, Vec<(u64, u32)>>>,
    // Dispatcher stats (§12 M3): a miss means the block had to be lifted. `Relaxed`
    // — these are counters, not synchronization.
    hits: AtomicU64,
    misses: AtomicU64,
    // Block-chaining "fires" counter (§12 M5, testing.md §8.2): a chained transfer
    // took the direct link-slot path instead of a cache lookup.
    chained: AtomicU64,
    // Superblocks compiled as multi-block regions (§12 M5-T3, testing.md §8.2).
    regions: AtomicU64,
    // Invalidation epoch (fast dispatch R1): bumped on every
    // `invalidate_overlapping` that drops a unit. Vcpu-local predictor state
    // (fast-resolve array, return ring) snapshots this in the dispatcher outer loop
    // and flushes when it changes — the coherence channel for state that cannot be
    // reached by the backend's `invalidate_links` (esp. cross-thread invalidation).
    epoch: AtomicU64,
    // IBTC descriptors (R4): immutable `[target, entry]` pairs published by pointer
    // into a per-site slot. Owned here (the dispatcher, in the core crate, allocates
    // them on a miss) and never freed before `Vm` drop, so a torn/late read of a
    // slot pointer can never dangle. The `Box` keeps each descriptor's address
    // stable across pushes. Only appended to; growth on a megamorphic site is capped
    // by the dispatcher, not here.
    #[allow(clippy::vec_box)]
    ibtc_descriptors: Mutex<Vec<Box<[u64; 2]>>>,
    // IBTC "fires" counter: descriptors published (§12 M5-style stat).
    ibtc_filled: AtomicU64,
    // Per-block execution counts for hotness-gated tier-up (FD tiering): a block
    // starts interpreted and is JIT-compiled only after it runs `tier_up_after`
    // times. Keyed by entry address; dropped alongside the block on invalidation.
    hotness: RwLock<HashMap<BlockKey, AtomicU32>>,
    // Cached region-candidacy decision per hot entry pc (task-156): `true` = this pc is
    // a multi-block loop that should tier up to a region at T2, `false` = tier the
    // single block at T1. Decided once (one `lift_region`) when a block first crosses
    // T1, so the dispatcher doesn't re-lift every dispatch while a loop warms toward T2.
    // Perf-only (both tiers are correct); a stale entry after SMC just re-decides on the
    // fresh block. Keyed by entry address.
    region_decision: RwLock<HashMap<BlockKey, bool>>,
    // Blocks whose background tier-up compile is in flight (bg-tier BGT-1, doc-27
    // D4): a hot block is submitted to the backend's compiler thread once and stays
    // here until the completion is published (or rejected), so a block running many
    // times before its compile lands isn't re-submitted every dispatch. Cleared on
    // invalidation so a dropped block's marker never wedges a re-lift. Lock order:
    // spans -> map -> hotness -> tier_pending (this is the innermost).
    tier_pending: Mutex<HashSet<BlockKey>>,
    // Background tier-up "fires" counters (doc-27 D6): a completion published into
    // the cache, or rejected (epoch moved / block gone) at publish time.
    tier_bg_published: AtomicU64,
    tier_bg_rejected: AtomicU64,
}

impl TranslationCache {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
            spans: RwLock::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            chained: AtomicU64::new(0),
            regions: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            ibtc_descriptors: Mutex::new(Vec::new()),
            ibtc_filled: AtomicU64::new(0),
            hotness: RwLock::new(HashMap::new()),
            region_decision: RwLock::new(HashMap::new()),
            tier_pending: Mutex::new(HashSet::new()),
            tier_bg_published: AtomicU64::new(0),
            tier_bg_rejected: AtomicU64::new(0),
        }
    }

    /// Increment a block's execution count and return the new value (FD tiering).
    /// Called on each dispatch of an interpreted block; when it reaches the Vm's
    /// `tier_up_after` the block is JIT-compiled. The count is an `AtomicU32`, so once
    /// a block's entry exists the bump takes only a **read** lock — concurrent vcpus
    /// running the same pre-hot block no longer serialize on a write lock. Only the
    /// first sight of a block takes the write lock to insert the counter.
    pub fn bump_hotness(&self, key: BlockKey) -> u32 {
        if let Some(c) = self.hotness.read().unwrap().get(&key) {
            return c.fetch_add(1, Ordering::Relaxed) + 1;
        }
        self.hotness
            .write()
            .unwrap()
            .entry(key)
            .or_insert_with(|| AtomicU32::new(0))
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }

    /// The cached region-candidacy decision for `key` (task-156), or `None` if this block
    /// hasn't been decided yet. Read on the hot path while a loop warms; a `read` lock.
    pub fn region_decision(&self, key: BlockKey) -> Option<bool> {
        self.region_decision.read().unwrap().get(&key).copied()
    }

    /// Record `key`'s region-candidacy decision (task-156) — done once, when the block
    /// first crosses T1, so the dispatcher never re-lifts to re-decide.
    pub fn set_region_decision(&self, key: BlockKey, candidate: bool) {
        self.region_decision.write().unwrap().insert(key, candidate);
    }

    /// Replace a cached block's materialization (interpreted → compiled) in place,
    /// re-establishing its guest `span` so SMC invalidation still finds it and
    /// dropping the now-useless hotness counter. Returns `false` (and changes
    /// nothing) if an `invalidate_overlapping` dropped the unit since the caller
    /// snapshotted `since_epoch` — that means the block's page was written under us,
    /// so the freshly compiled block is already stale. Resurrecting it here (map
    /// entry with no span) would make it permanently invisible to future
    /// invalidation (#3); the caller must re-lift instead.
    #[must_use]
    pub fn upgrade(
        &self,
        key: BlockKey,
        block: CachedBlock,
        span: (u64, u32),
        since_epoch: u64,
    ) -> bool {
        // Lock order matches `invalidate_overlapping` (spans → map → hotness) so the
        // two can't deadlock. Holding spans+map write locks serializes this against a
        // concurrent SMC drop, which bumps `epoch` while holding those same locks —
        // so the epoch check below cannot race a half-completed invalidation.
        let mut spans = self.spans.write().unwrap();
        let mut map = self.map.write().unwrap();
        if self.epoch.load(Ordering::Acquire) != since_epoch {
            return false;
        }
        spans.insert(key, vec![span]);
        map.insert(key, block);
        self.hotness.write().unwrap().remove(&key);
        true
    }

    /// Publish a background-compiled unit that may span **multiple** sub-blocks (bg-tier
    /// region tier-up, BGT-6): [`upgrade`](Self::upgrade)'s epoch-reject + hotness-drop,
    /// but storing a span *list* and re-tagging every span's pages via `on_mark` (as
    /// [`insert`](Self::insert) does) under the spans lock (#12). A block finish passes a
    /// one-element list; `mark_code` is idempotent, so re-tagging its already-tagged page
    /// is harmless. Returns `false` (changing nothing) if an SMC drop moved the epoch
    /// since `since_epoch` — the caller must re-lift instead of resurrecting a stale unit.
    #[must_use]
    pub fn upgrade_region(
        &self,
        key: BlockKey,
        block: CachedBlock,
        spans: Vec<(u64, u32)>,
        since_epoch: u64,
        on_mark: impl FnOnce(&[(u64, u32)]),
    ) -> bool {
        let mut sp = self.spans.write().unwrap();
        let mut mp = self.map.write().unwrap();
        if self.epoch.load(Ordering::Acquire) != since_epoch {
            return false;
        }
        on_mark(&spans);
        mp.insert(key, block);
        sp.insert(key, spans);
        self.hotness.write().unwrap().remove(&key);
        true
    }

    /// Allocate an immutable IBTC descriptor `{target, entry}` (R4) and return its
    /// stable heap address for the compiled code to load. The descriptor is never
    /// mutated (a new target gets a new descriptor) and never freed before `Vm`
    /// drop. The pointer publication into the slot must be a **`Release`** store (see
    /// the `RET_IBTC_MISS` site in `vm.rs`): the two field writes above happen-before
    /// the pointer becomes visible, pairing with the reader's address dependency
    /// (release/consume). A `Relaxed` publish would let a weakly-ordered host expose
    /// the pointer before the `{target, entry}` fields — a torn payload, not just a
    /// torn scalar.
    pub fn alloc_ibtc_descriptor(&self, target: u64, entry: CompiledPtr) -> u64 {
        let b = Box::new([target, entry.0 as u64]);
        let addr = &*b as *const [u64; 2] as u64;
        self.ibtc_descriptors.lock().unwrap().push(b);
        self.ibtc_filled.fetch_add(1, Ordering::Relaxed);
        addr
    }

    /// IBTC descriptors published (the R4 "fires" counter).
    pub fn ibtc_filled(&self) -> u64 {
        self.ibtc_filled.load(Ordering::Relaxed)
    }

    /// Current invalidation epoch (R1). Monotonic; bumped whenever
    /// `invalidate_overlapping` drops at least one unit. A vcpu snapshots this and
    /// flushes its local predictor caches when the value changes.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Look up a block, recording a hit (found) or miss (must lift).
    pub fn get(&self, key: BlockKey) -> Option<CachedBlock> {
        let found = self.map.read().unwrap().get(&key).cloned();
        if found.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        found
    }

    /// Blocks served from the cache (no re-lift).
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Blocks that had to be lifted (distinct addresses, plus any re-lift after
    /// invalidation).
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Add `n` chained (link-slot) block transfers, once per `Vcpu::run` rather than
    /// once per transfer.
    ///
    /// Chaining is the engine's hottest event — a real game measured ~1,000,000 of
    /// them per frame against ~5,000 indirect-branch misses — so a shared
    /// `fetch_add` here is a locked RMW on a line every vcpu writes, on the hottest
    /// path there is. That is the same contention `Vcpu::fast_hits` already avoids by
    /// counting privately; this counter simply had not been given the same treatment.
    /// The vcpu accumulates in a plain `u64` and flushes once at the end of a run.
    pub fn add_chained(&self, n: u64) {
        if n != 0 {
            self.chained.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Chained transfers taken (the block-chaining "fires" counter, §12 M5).
    pub fn chained(&self) -> u64 {
        self.chained.load(Ordering::Relaxed)
    }

    /// Record a superblock (multi-block region) compilation.
    pub fn record_region(&self) {
        self.regions.fetch_add(1, Ordering::Relaxed);
    }

    /// Superblocks compiled as regions (M5-T3 "fires" counter).
    pub fn regions(&self) -> u64 {
        self.regions.load(Ordering::Relaxed)
    }

    /// Claim `pc` for a background tier-up (bg-tier BGT-1, doc-27 D4). Returns
    /// `true` if this caller now owns the in-flight slot (submit the compile);
    /// `false` if a compile for `pc` is already pending (skip — don't re-submit).
    /// Pairs with [`end_tier_up`](Self::end_tier_up) once the completion is
    /// published, rejected, or the block is invalidated.
    pub fn try_begin_tier_up(&self, key: BlockKey) -> bool {
        self.tier_pending.lock().unwrap().insert(key)
    }

    /// Release `pc`'s in-flight marker (idempotent — a no-op if already clear, so a
    /// publish and a racing invalidation can both call it). See
    /// [`try_begin_tier_up`](Self::try_begin_tier_up).
    pub fn end_tier_up(&self, key: BlockKey) {
        self.tier_pending.lock().unwrap().remove(&key);
    }

    /// Number of background tier-up compiles currently in flight (observability /
    /// test invariant: this must return to 0 once the queue drains — no stuck
    /// marker after a race).
    pub fn tier_pending_len(&self) -> usize {
        self.tier_pending.lock().unwrap().len()
    }

    /// Record a background tier-up completion published into the cache (the D6
    /// "fires" counter).
    pub fn record_tier_bg_published(&self) {
        self.tier_bg_published.fetch_add(1, Ordering::Relaxed);
    }

    /// Background tier-up completions published (interp → compiled swap landed).
    pub fn tier_bg_published(&self) -> u64 {
        self.tier_bg_published.load(Ordering::Relaxed)
    }

    /// Record a background tier-up completion rejected at publish (epoch moved or
    /// the block was invalidated while its compile was in flight).
    pub fn record_tier_bg_rejected(&self) {
        self.tier_bg_rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Background tier-up completions dropped at publish time.
    pub fn tier_bg_rejected(&self) -> u64 {
        self.tier_bg_rejected.load(Ordering::Relaxed)
    }

    /// Cache a materialized unit keyed by `pc`, covering the given guest byte
    /// `spans` (one for a single block, several for a superblock). `on_mark` tags
    /// the covered code pages (§10) and runs **under the spans lock**, so it is
    /// serialized against `invalidate_overlapping`'s page-tag clear — a concurrent
    /// SMC drop can't wipe the tag of a block being inserted here (#12).
    pub fn insert(
        &self,
        key: BlockKey,
        block: CachedBlock,
        spans: Vec<(u64, u32)>,
        on_mark: impl FnOnce(&[(u64, u32)]),
    ) {
        let mut sp = self.spans.write().unwrap();
        let mut mp = self.map.write().unwrap();
        on_mark(&spans);
        mp.insert(key, block);
        sp.insert(key, spans);
    }

    /// SMC invalidation (§10): drop every cached unit *any* of whose guest spans
    /// overlaps `[lo, hi)` and report their entry addresses. A linear scan — only
    /// run when a write actually lands on a code page (rare), so no index needed.
    ///
    /// `[lo, hi)` is one guest page. `on_clear_page` clears that page's SMC tag
    /// (§10) and runs **under the spans lock**, right after the overlapping units are
    /// removed. This closes the #12 race: because it removes *every* span overlapping
    /// the page before clearing, no live block's page is ever left cleared, and
    /// because both this clear and `insert`'s `on_mark` hold the spans lock, a
    /// concurrent insert can't interleave — it either publishes its span before us
    /// (we then drop it as a victim) or marks the tag after us (so the tag survives).
    /// The old two-step `invalidate` then `clear_code_page` left a window where the
    /// insert's mark landed in between and got wiped.
    pub fn invalidate_overlapping(
        &self,
        lo: u64,
        hi: u64,
        on_clear_page: impl FnOnce(),
    ) -> Vec<BlockKey> {
        let mut spans = self.spans.write().unwrap();
        let mut map = self.map.write().unwrap();
        // SMC is address-scoped: a write to `[lo, hi)` drops every overlapping unit
        // regardless of the mode it was lifted under (§17.4 — the key carries the mode,
        // but invalidation matches by span address across all modes).
        let victims: Vec<BlockKey> = spans
            .iter()
            .filter(|(_, ranges)| {
                ranges
                    .iter()
                    .any(|(start, len)| *start < hi && lo < *start + *len as u64)
            })
            .map(|(entry, _)| *entry)
            .collect();
        if !victims.is_empty() {
            let mut hotness = self.hotness.write().unwrap();
            // Innermost lock (spans -> map -> hotness -> tier_pending): drop any
            // in-flight background tier-up marker for a victim so a later completion
            // is rejected and the block can be freely re-lifted (bg-tier BGT-1).
            let mut pending = self.tier_pending.lock().unwrap();
            for entry in &victims {
                spans.remove(entry);
                map.remove(entry);
                hotness.remove(entry);
                pending.remove(entry);
            }
        }
        // Every unit touching the page is now gone, so the tag is stale — clear it
        // here, still holding the spans lock (#12).
        on_clear_page();
        // Bump the epoch so vcpu-local predictors flush (R1). Only on a real drop —
        // a write to a data page (no victims) must not perturb anything. `Release`
        // pairs with the `Acquire` load in `epoch()`.
        if !victims.is_empty() {
            self.epoch.fetch_add(1, Ordering::Release);
        }
        victims
    }
}

impl Default for TranslationCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled() -> CachedBlock {
        CachedBlock::Compiled {
            entry: CompiledPtr(std::ptr::null()),
        }
    }

    /// A Long64 key at `addr` — the shape most cache tests key by.
    fn k(addr: u64) -> BlockKey {
        BlockKey::new(addr, CpuMode::Long64)
    }

    /// No race: a tier-up whose epoch snapshot still matches commits, and the block
    /// stays invalidatable (its span is present).
    #[test]
    fn tier_up_commits_and_keeps_span() {
        let c = TranslationCache::new();
        c.insert(k(0x1000), compiled(), vec![(0x1000, 4)], |_| {});
        let e = c.epoch();
        assert!(c.upgrade(k(0x1000), compiled(), (0x1000, 4), e));
        // A later write to the block's page must still find it via its span.
        assert_eq!(
            c.invalidate_overlapping(0x1000, 0x1004, || {}),
            vec![k(0x1000)]
        );
    }

    /// The #3 race: an `invalidate_overlapping` drops the unit (bumping the epoch)
    /// between the tier-up's epoch snapshot and its `upgrade`. `upgrade` must reject
    /// the stale compile rather than resurrect a spanless — permanently
    /// uninvalidatable — block.
    #[test]
    fn tier_up_rejected_when_invalidated_mid_upgrade() {
        let c = TranslationCache::new();
        c.insert(k(0x1000), compiled(), vec![(0x1000, 4)], |_| {});
        let e = c.epoch(); // snapshot BEFORE the racing invalidation

        // Concurrent SMC drop: removes the unit and bumps the epoch.
        assert_eq!(
            c.invalidate_overlapping(0x1000, 0x1004, || {}),
            vec![k(0x1000)]
        );
        assert!(c.get(k(0x1000)).is_none(), "invalidation dropped the block");

        // The stale tier-up now tries to commit with the pre-drop epoch.
        assert!(
            !c.upgrade(k(0x1000), compiled(), (0x1000, 4), e),
            "upgrade must reject a compile the SMC drop raced past"
        );
        assert!(
            c.get(k(0x1000)).is_none(),
            "must not resurrect the block (a spanless entry would be permanent)"
        );
    }

    /// bg-tier BGT-1 (D4): the background tier-up in-flight set. A pc is claimable
    /// once; a second claim while pending is rejected (no double-submit); `end_tier_up`
    /// releases it and is idempotent (a publish and a racing invalidation may both
    /// call it).
    #[test]
    fn tier_pending_set_transitions() {
        let c = TranslationCache::new();

        assert!(c.try_begin_tier_up(k(0x1000)), "first claim owns the slot");
        assert!(
            !c.try_begin_tier_up(k(0x1000)),
            "double-begin rejected while pending"
        );

        c.end_tier_up(k(0x1000));
        assert!(c.try_begin_tier_up(k(0x1000)), "claimable again after end");

        // Idempotent: extra ends are harmless, and a distinct pc is independent.
        c.end_tier_up(k(0x1000));
        c.end_tier_up(k(0x1000));
        assert!(
            c.try_begin_tier_up(k(0x1000)),
            "still claimable after double-end"
        );
        assert!(
            c.try_begin_tier_up(k(0x2000)),
            "a different pc has its own slot"
        );
        c.end_tier_up(k(0x1000));
        c.end_tier_up(k(0x2000));
    }

    /// bg-tier BGT-1: an SMC drop clears a victim's in-flight marker, so a background
    /// compile that lands afterward finds no marker (its publish is separately
    /// epoch-rejected) and the pc is freely re-claimable — a dropped block never
    /// wedges its pending slot.
    #[test]
    fn invalidate_clears_pending_marker() {
        let c = TranslationCache::new();
        c.insert(k(0x1000), compiled(), vec![(0x1000, 4)], |_| {});
        assert!(c.try_begin_tier_up(k(0x1000)), "claim the in-flight slot");

        assert_eq!(
            c.invalidate_overlapping(0x1000, 0x1004, || {}),
            vec![k(0x1000)]
        );

        assert!(
            c.try_begin_tier_up(k(0x1000)),
            "invalidation cleared the marker, so the slot is free again"
        );
        c.end_tier_up(k(0x1000));
    }

    /// bg-tier BGT-1: the D6 "fires" counters start at zero and count monotonically.
    #[test]
    fn tier_bg_counters_count() {
        let c = TranslationCache::new();
        assert_eq!((c.tier_bg_published(), c.tier_bg_rejected()), (0, 0));
        c.record_tier_bg_published();
        c.record_tier_bg_published();
        c.record_tier_bg_rejected();
        assert_eq!((c.tier_bg_published(), c.tier_bg_rejected()), (2, 1));
    }

    /// §17.4: the block key carries the decode mode, so the same guest address holds
    /// **distinct** translations under Long64 vs Compat32 — they never alias.
    #[test]
    fn same_addr_distinct_entry_per_mode() {
        let c = TranslationCache::new();
        let long = BlockKey::new(0x1000, CpuMode::Long64);
        let compat = BlockKey::new(0x1000, CpuMode::Compat32);
        assert_ne!(long, compat);

        c.insert(long, compiled(), vec![(0x1000, 4)], |_| {});
        assert!(c.get(long).is_some(), "the Long64 translation is present");
        assert!(
            c.get(compat).is_none(),
            "the same address in Compat32 is a distinct, absent key"
        );

        c.insert(compat, compiled(), vec![(0x1000, 4)], |_| {});
        assert!(
            c.get(long).is_some() && c.get(compat).is_some(),
            "both modes coexist at one address"
        );

        // SMC to the page is address-scoped: it drops both mode's translations.
        let victims = c.invalidate_overlapping(0x1000, 0x1004, || {});
        assert_eq!(victims.len(), 2, "SMC drops every mode at the address");
        assert!(c.get(long).is_none() && c.get(compat).is_none());
    }

    /// #12 wiring: `insert` tags the page and `invalidate_overlapping` clears it, both
    /// through their callbacks under the spans lock — so an insert's mark and an SMC
    /// drop's clear can't interleave and wipe a live block's tag.
    #[test]
    fn insert_marks_and_invalidate_clears_page_tag() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        let c = TranslationCache::new();
        let tag = AtomicBool::new(false);
        c.insert(k(0x1000), compiled(), vec![(0x1000, 4)], |_| {
            tag.store(true, Relaxed)
        });
        assert!(tag.load(Relaxed), "insert tagged the page");
        let v = c.invalidate_overlapping(0x1000, 0x2000, || tag.store(false, Relaxed));
        assert_eq!(v, vec![k(0x1000)]);
        assert!(!tag.load(Relaxed), "invalidate cleared the page tag");
    }
}
