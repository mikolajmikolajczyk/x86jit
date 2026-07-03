//! Translation cache keyed by guest address (§9.1).

use std::collections::HashMap;
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
}

impl TranslationCache {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
    }

    pub fn get(&self, pc: u64) -> Option<CachedBlock> {
        self.map.read().unwrap().get(&pc).cloned()
    }

    pub fn insert(&self, pc: u64, block: CachedBlock) {
        self.map.write().unwrap().insert(pc, block);
    }

    /// Drop a cache entry (SMC invalidation, §10).
    pub fn invalidate(&self, pc: u64) {
        self.map.write().unwrap().remove(&pc);
    }
}

impl Default for TranslationCache {
    fn default() -> Self {
        Self::new()
    }
}
