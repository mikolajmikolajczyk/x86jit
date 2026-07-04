//! Translation cache keyed by guest address (§9.1).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

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
    Compiled { entry: CompiledPtr, guest_len: u32 },
}

/// Shared translation cache. Cloned-out on `get` so no lock guard is held
/// across block execution (which may mutate memory -> SMC invalidation) (§9.2).
pub struct TranslationCache {
    // SEAM (§17.4): key is u64 (guest address). If CPU modes are ever added,
    // switch to BlockKey { guest_addr, mode } — today mode is always Long64.
    map: RwLock<HashMap<u64, CachedBlock>>,
    // Guest byte span of each cached block (start -> length) for SMC range
    // invalidation (§10): a write overlapping any block's `[start, start+len)`
    // drops it. Kept in lockstep with `map`.
    spans: RwLock<HashMap<u64, u32>>,
    // Dispatcher stats (§12 M3): a miss means the block had to be lifted. `Relaxed`
    // — these are counters, not synchronization.
    hits: AtomicU64,
    misses: AtomicU64,
    // Block-chaining "fires" counter (§12 M5, testing.md §8.2): a chained transfer
    // took the direct link-slot path instead of a cache lookup.
    chained: AtomicU64,
}

impl TranslationCache {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
            spans: RwLock::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            chained: AtomicU64::new(0),
        }
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

    /// Cache a materialized block spanning `guest_len` guest bytes from `pc`.
    pub fn insert(&self, pc: u64, block: CachedBlock, guest_len: u32) {
        self.map.write().unwrap().insert(pc, block);
        self.spans.write().unwrap().insert(pc, guest_len);
    }

    /// SMC invalidation (§10): drop every block whose guest span overlaps
    /// `[lo, hi)` and report their start addresses. A linear scan of the span
    /// table — only ever run when a write actually lands on a code page, which is
    /// rare, so no per-block index is warranted.
    pub fn invalidate_overlapping(&self, lo: u64, hi: u64) -> Vec<u64> {
        let mut spans = self.spans.write().unwrap();
        let mut map = self.map.write().unwrap();
        let victims: Vec<u64> = spans
            .iter()
            .filter(|(start, len)| **start < hi && lo < **start + **len as u64)
            .map(|(start, _)| *start)
            .collect();
        for start in &victims {
            spans.remove(start);
            map.remove(start);
        }
        victims
    }
}

impl Default for TranslationCache {
    fn default() -> Self {
        Self::new()
    }
}
