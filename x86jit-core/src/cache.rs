//! Translation cache keyed by guest address (§9.1).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::ir::IrBlock;

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
    // SEAM (§17.4): key is u64 (guest address). If CPU modes are ever added,
    // switch to BlockKey { guest_addr, mode } — today mode is always Long64.
    map: RwLock<HashMap<u64, CachedBlock>>,
    // Guest byte spans of each cached unit, keyed by its entry address. A single
    // block has one `(start, len)`; a superblock (M5-T3) has one per sub-block,
    // possibly non-contiguous. A write overlapping ANY span drops the whole unit
    // (§10). Kept in lockstep with `map`.
    spans: RwLock<HashMap<u64, Vec<(u64, u32)>>>,
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
    hotness: RwLock<HashMap<u64, AtomicU32>>,
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
        }
    }

    /// Increment a block's execution count and return the new value (FD tiering).
    /// Called on each dispatch of an interpreted block; when it reaches the Vm's
    /// `tier_up_after` the block is JIT-compiled. The count is an `AtomicU32`, so once
    /// a block's entry exists the bump takes only a **read** lock — concurrent vcpus
    /// running the same pre-hot block no longer serialize on a write lock. Only the
    /// first sight of a block takes the write lock to insert the counter.
    pub fn bump_hotness(&self, pc: u64) -> u32 {
        if let Some(c) = self.hotness.read().unwrap().get(&pc) {
            return c.fetch_add(1, Ordering::Relaxed) + 1;
        }
        self.hotness
            .write()
            .unwrap()
            .entry(pc)
            .or_insert_with(|| AtomicU32::new(0))
            .fetch_add(1, Ordering::Relaxed)
            + 1
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
    pub fn upgrade(&self, pc: u64, block: CachedBlock, span: (u64, u32), since_epoch: u64) -> bool {
        // Lock order matches `invalidate_overlapping` (spans → map → hotness) so the
        // two can't deadlock. Holding spans+map write locks serializes this against a
        // concurrent SMC drop, which bumps `epoch` while holding those same locks —
        // so the epoch check below cannot race a half-completed invalidation.
        let mut spans = self.spans.write().unwrap();
        let mut map = self.map.write().unwrap();
        if self.epoch.load(Ordering::Acquire) != since_epoch {
            return false;
        }
        spans.insert(pc, vec![span]);
        map.insert(pc, block);
        self.hotness.write().unwrap().remove(&pc);
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
    pub fn get(&self, pc: u64) -> Option<CachedBlock> {
        let found = self.map.read().unwrap().get(&pc).cloned();
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

    /// Record a chained (link-slot) block transfer.
    pub fn record_chain(&self) {
        self.chained.fetch_add(1, Ordering::Relaxed);
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

    /// Cache a materialized unit keyed by `pc`, covering the given guest byte
    /// `spans` (one for a single block, several for a superblock). `on_mark` tags
    /// the covered code pages (§10) and runs **under the spans lock**, so it is
    /// serialized against `invalidate_overlapping`'s page-tag clear — a concurrent
    /// SMC drop can't wipe the tag of a block being inserted here (#12).
    pub fn insert(
        &self,
        pc: u64,
        block: CachedBlock,
        spans: Vec<(u64, u32)>,
        on_mark: impl FnOnce(&[(u64, u32)]),
    ) {
        let mut sp = self.spans.write().unwrap();
        let mut mp = self.map.write().unwrap();
        on_mark(&spans);
        mp.insert(pc, block);
        sp.insert(pc, spans);
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
    ) -> Vec<u64> {
        let mut spans = self.spans.write().unwrap();
        let mut map = self.map.write().unwrap();
        let victims: Vec<u64> = spans
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
            for entry in &victims {
                spans.remove(entry);
                map.remove(entry);
                hotness.remove(entry);
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

    /// No race: a tier-up whose epoch snapshot still matches commits, and the block
    /// stays invalidatable (its span is present).
    #[test]
    fn tier_up_commits_and_keeps_span() {
        let c = TranslationCache::new();
        c.insert(0x1000, compiled(), vec![(0x1000, 4)], |_| {});
        let e = c.epoch();
        assert!(c.upgrade(0x1000, compiled(), (0x1000, 4), e));
        // A later write to the block's page must still find it via its span.
        assert_eq!(
            c.invalidate_overlapping(0x1000, 0x1004, || {}),
            vec![0x1000]
        );
    }

    /// The #3 race: an `invalidate_overlapping` drops the unit (bumping the epoch)
    /// between the tier-up's epoch snapshot and its `upgrade`. `upgrade` must reject
    /// the stale compile rather than resurrect a spanless — permanently
    /// uninvalidatable — block.
    #[test]
    fn tier_up_rejected_when_invalidated_mid_upgrade() {
        let c = TranslationCache::new();
        c.insert(0x1000, compiled(), vec![(0x1000, 4)], |_| {});
        let e = c.epoch(); // snapshot BEFORE the racing invalidation

        // Concurrent SMC drop: removes the unit and bumps the epoch.
        assert_eq!(
            c.invalidate_overlapping(0x1000, 0x1004, || {}),
            vec![0x1000]
        );
        assert!(c.get(0x1000).is_none(), "invalidation dropped the block");

        // The stale tier-up now tries to commit with the pre-drop epoch.
        assert!(
            !c.upgrade(0x1000, compiled(), (0x1000, 4), e),
            "upgrade must reject a compile the SMC drop raced past"
        );
        assert!(
            c.get(0x1000).is_none(),
            "must not resurrect the block (a spanless entry would be permanent)"
        );
    }

    /// #12 wiring: `insert` tags the page and `invalidate_overlapping` clears it, both
    /// through their callbacks under the spans lock — so an insert's mark and an SMC
    /// drop's clear can't interleave and wipe a live block's tag.
    #[test]
    fn insert_marks_and_invalidate_clears_page_tag() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        let c = TranslationCache::new();
        let tag = AtomicBool::new(false);
        c.insert(0x1000, compiled(), vec![(0x1000, 4)], |_| {
            tag.store(true, Relaxed)
        });
        assert!(tag.load(Relaxed), "insert tagged the page");
        let v = c.invalidate_overlapping(0x1000, 0x2000, || tag.store(false, Relaxed));
        assert_eq!(v, vec![0x1000]);
        assert!(!tag.load(Relaxed), "invalidate cleared the page tag");
    }
}
