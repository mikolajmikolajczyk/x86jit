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

    /// Record a superblock (multi-block region) compilation.
    pub fn record_region(&self) {
        self.regions.fetch_add(1, Ordering::Relaxed);
    }

    /// Superblocks compiled as regions (M5-T3 "fires" counter).
    pub fn regions(&self) -> u64 {
        self.regions.load(Ordering::Relaxed)
    }

    /// Cache a materialized unit keyed by `pc`, covering the given guest byte
    /// `spans` (one for a single block, several for a superblock).
    pub fn insert(&self, pc: u64, block: CachedBlock, spans: Vec<(u64, u32)>) {
        self.map.write().unwrap().insert(pc, block);
        self.spans.write().unwrap().insert(pc, spans);
    }

    /// SMC invalidation (§10): drop every cached unit *any* of whose guest spans
    /// overlaps `[lo, hi)` and report their entry addresses. A linear scan — only
    /// run when a write actually lands on a code page (rare), so no index needed.
    pub fn invalidate_overlapping(&self, lo: u64, hi: u64) -> Vec<u64> {
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
        for entry in &victims {
            spans.remove(entry);
            map.remove(entry);
        }
        victims
    }
}

impl Default for TranslationCache {
    fn default() -> Self {
        Self::new()
    }
}
