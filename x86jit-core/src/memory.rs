//! Guest memory model (§4.1, §4.2, §8.1).

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::ir::RmwOp;

/// Memory model selection. Start with `Flat`; add `SoftMmu` when the guest
/// uses a sparse, high address space (§4.1).
#[derive(Clone, Copy, Debug)]
pub enum MemoryModel {
    /// One contiguous host buffer of `size` bytes representing guest space
    /// `[0, size)`. Translation is `host_base + guest_addr`. `map()` only
    /// tags regions; it does not allocate. Addresses `>= size` are unmapped.
    Flat { size: u64 },
    /// Sparse address space via a page/region table. `map()` allocates pages.
    SoftMmu,
}

/// Access protection for a mapped region (§4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prot {
    R,
    RW,
    RX,
    RWX,
}

/// Region behavior (§4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind {
    /// Ordinary RAM. Access is inlined into generated code — no trap-out.
    Ram,
    /// Trapped region: every access yields `Exit::MmioRead`/`MmioWrite`.
    Trap,
}

/// Why a memory access could not complete inline (§8.1). The interpreter and
/// codegen turn this into the appropriate `Exit`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MemTrap {
    Unmapped,
    Mmio,
}

#[derive(Debug)]
pub enum MapError {
    Overlap,
    OutOfBounds,
    BadAlign,
}

#[derive(Debug)]
pub enum MemError {
    Unmapped,
    Protection,
}

/// Guest memory. Owns the host-side backing buffer and region metadata.
///
/// The `host_base` of the backing buffer is what the JIT adds to a guest
/// address to inline RAM access (§8.2.1).
///
/// **Interior mutability (§8 pitfall).** Guest RAM is written through `&self`,
/// NOT `&mut self`: one `Memory` is shared across vcpus, which write concurrently
/// and race exactly like real hardware — ordering comes from TSO barriers
/// (§8.2.3, §11), not from Rust's `&mut`. This is the one place the core is
/// deliberately `unsafe`. `CpuState` stays `&mut` and per-vcpu; only `Memory` is shared.
/// A mapped guest region. In `Flat` this only TAGS a slice of the pre-allocated
/// backing buffer with permissions/kind — it does not own memory (§4.1).
#[derive(Clone)]
struct Region {
    start: u64,
    size: usize,
    // `prot` is recorded but not yet enforced: the scalar read/write contract only
    // distinguishes mapped/unmapped/MMIO (`MemTrap` has no protection variant), so
    // W-on-RX etc. is deferred. `kind` routes RAM vs Trap (MMIO). (§4.2)
    #[allow(dead_code)]
    prot: Prot,
    kind: RegionKind,
}

/// Guest page size for SMC tracking (§10): the granularity at which a page is
/// tagged "backs translated code" and at which a code-overlapping write triggers
/// cache invalidation. Independent of any host page size.
pub const CODE_PAGE_BITS: u32 = 12;
const CODE_PAGE_SIZE: u64 = 1 << CODE_PAGE_BITS;

pub struct Memory {
    // Selects the mapping strategy in `map()`; retained for the SoftMmu switch (§4.1).
    model: MemoryModel,
    // Flat backing store behind UnsafeCell so `write(&self)` is sound; SoftMmu
    // region table comes later. Access must be bounds-checked (§8.2.3) — no raw
    // out-of-range indexing, that would be host UB.
    backing: UnsafeCell<Box<[u8]>>,
    // Region tags (prot/kind + bounds). `map()`/`unmap()` mutate this through
    // `&mut self` before execution; per-access lookups read it through `&self`.
    regions: Vec<Region>,
    // SMC tracking (§10). `code_page[p]` = page `p` backs a translated block, so a
    // write to it must invalidate the cache. Atomic (not `&mut`) because it's set
    // through `&self` at block-resolve time and read on every store — the same
    // shared-through-`&self` discipline as `backing`. `dirty` collects code pages
    // written since the last drain; `dirty_flag` lets the hot path skip the lock.
    code_page: Box<[AtomicBool]>,
    dirty: Mutex<Vec<u64>>,
    dirty_flag: AtomicBool,
}

// SAFETY: concurrent guest stores are intended to race like real hardware; the
// guest's expected ordering is provided by emitted TSO barriers, not by Rust
// aliasing rules. No host-side invariant is broken by concurrent access to the
// flat byte buffer (bounds-checked per access).
unsafe impl Sync for Memory {}

impl Memory {
    pub fn new(model: MemoryModel) -> Self {
        let backing: Box<[u8]> = match &model {
            MemoryModel::Flat { size } => vec![0u8; *size as usize].into_boxed_slice(),
            MemoryModel::SoftMmu => Box::new([]),
        };
        let pages = backing.len().div_ceil(CODE_PAGE_SIZE as usize);
        let code_page = (0..pages).map(|_| AtomicBool::new(false)).collect();
        Self {
            model,
            backing: UnsafeCell::new(backing),
            regions: Vec::new(),
            code_page,
            dirty: Mutex::new(Vec::new()),
            dirty_flag: AtomicBool::new(false),
        }
    }

    /// Deep-copy this memory into an independent `Memory` with identical contents
    /// and region tags but its own (empty) SMC/dirty state — the guest-agnostic
    /// primitive behind `fork` (§4.2). The child's byte buffer is a fresh
    /// allocation; writes on either side don't affect the other.
    pub fn deep_copy(&self) -> Memory {
        // SAFETY: we read the backing buffer to clone it. This is a snapshot at a
        // quiescent point (between guest steps); no concurrent vcpu writes the
        // parent during a fork.
        let bytes: Box<[u8]> = unsafe { (*self.backing.get()).clone() };
        let pages = bytes.len().div_ceil(CODE_PAGE_SIZE as usize);
        Memory {
            model: self.model,
            backing: UnsafeCell::new(bytes),
            regions: self.regions.clone(),
            code_page: (0..pages).map(|_| AtomicBool::new(false)).collect(),
            dirty: Mutex::new(Vec::new()),
            dirty_flag: AtomicBool::new(false),
        }
    }

    /// Tag every page spanned by `[addr, addr+len)` as backing translated code
    /// (§10). Called through `&self` when a block is cached, so a later store to
    /// the page is caught. Idempotent.
    pub fn mark_code(&self, addr: u64, len: u32) {
        let last = addr.saturating_add(len.max(1) as u64 - 1);
        for page in (addr >> CODE_PAGE_BITS)..=(last >> CODE_PAGE_BITS) {
            if let Some(bit) = self.code_page.get(page as usize) {
                bit.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Clear a page's code tag after its blocks have been invalidated (§10).
    pub fn clear_code_page(&self, page: u64) {
        if let Some(bit) = self.code_page.get(page as usize) {
            bit.store(false, Ordering::Relaxed);
        }
    }

    /// Note a store of `size` bytes at `addr`: if it lands on a code page, record
    /// the page(s) as dirty for the dispatcher to invalidate (§10). The common
    /// case (a non-code page) costs one relaxed atomic load and returns.
    fn note_write(&self, addr: u64, len: usize) {
        let last = addr.saturating_add(len.max(1) as u64 - 1);
        for page in (addr >> CODE_PAGE_BITS)..=(last >> CODE_PAGE_BITS) {
            let is_code = self
                .code_page
                .get(page as usize)
                .is_some_and(|b| b.load(Ordering::Relaxed));
            if is_code {
                self.dirty.lock().unwrap().push(page);
                self.dirty_flag.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Drain the set of code pages written since the last call (§10). Empty and
    /// lock-free in the common case (nothing self-modified).
    pub fn take_dirty_code(&self) -> Vec<u64> {
        if !self.dirty_flag.swap(false, Ordering::Relaxed) {
            return Vec::new();
        }
        std::mem::take(&mut *self.dirty.lock().unwrap())
    }

    /// Highest mapped guest address (exclusive end of the top region) strictly below
    /// `limit`, or 0 if nothing is mapped there. Lets an embedder place the heap just
    /// above a loaded image's segments instead of at a fixed guess (#14).
    pub fn highest_mapped_below(&self, limit: u64) -> u64 {
        self.regions
            .iter()
            .filter(|r| r.start < limit)
            .map(|r| r.start + r.size as u64)
            .max()
            .unwrap_or(0)
    }

    /// The mapped region wholly containing `[addr, addr + len)`, if any.
    fn region_for(&self, addr: u64, len: usize) -> Option<&Region> {
        let end = addr.checked_add(len as u64)?;
        self.regions
            .iter()
            .find(|r| r.start <= addr && end <= r.start + r.size as u64)
    }

    /// Base pointer of the guest RAM buffer (JIT inlines `host_base + addr`).
    pub fn host_base(&self) -> *const u8 {
        // SAFETY: pointer to the backing buffer's start; callers bounds-check.
        unsafe { (*self.backing.get()).as_ptr() }
    }

    /// Size of the flat backing buffer in bytes — the bound the JIT checks a guest
    /// address against before an inlined access (§8.2.3). The buffer is allocated
    /// from this value, so it equals `backing.len()`.
    pub fn size(&self) -> u64 {
        match self.model {
            MemoryModel::Flat { size } => size,
            MemoryModel::SoftMmu => 0,
        }
    }

    /// Reserve a region. In `Flat` this only tags `[guest_addr, guest_addr+size)`
    /// with prot/kind and bounds-checks it against the backing buffer — it does
    /// NOT allocate (`map(high_addr)` in Flat is a tag, not a 128 TB alloc). (§4.1)
    pub fn map(
        &mut self,
        guest_addr: u64,
        size: usize,
        prot: Prot,
        kind: RegionKind,
    ) -> Result<(), MapError> {
        match self.model {
            MemoryModel::Flat { size: total } => {
                let end = guest_addr
                    .checked_add(size as u64)
                    .ok_or(MapError::OutOfBounds)?;
                if end > total {
                    return Err(MapError::OutOfBounds);
                }
                let overlaps = self
                    .regions
                    .iter()
                    .any(|r| guest_addr < r.start + r.size as u64 && r.start < end);
                if overlaps {
                    return Err(MapError::Overlap);
                }
                self.regions.push(Region {
                    start: guest_addr,
                    size,
                    prot,
                    kind,
                });
                Ok(())
            }
            MemoryModel::SoftMmu => todo!("SoftMmu: allocate pages for the region (§4.1)"),
        }
    }

    /// Load bytes into an already-mapped region (e.g. an ELF segment). Host-side
    /// loader path: it bypasses guest `Prot` (you write code into an RX region),
    /// so it only checks that the range is mapped. (§4.2)
    pub fn write_bytes(&mut self, guest_addr: u64, bytes: &[u8]) -> Result<(), MemError> {
        if self.region_for(guest_addr, bytes.len()).is_none() {
            return Err(MemError::Unmapped);
        }
        let start = guest_addr as usize;
        // `&mut self` is exclusive, so no interior-mutability dance is needed;
        // the range sits inside a mapped region that `map()` already bounds-checked.
        let backing = self.backing.get_mut();
        backing[start..start + bytes.len()].copy_from_slice(bytes);
        // SMC: an embedder write (loader, syscall passthrough) over a code page
        // must invalidate too (§10).
        self.note_write(guest_addr, bytes.len());
        Ok(())
    }

    /// Guest-side wide read for the x87 helpers (`&self`, interior-mutable model):
    /// copy `buf.len()` bytes out of a mapped **RAM** region. `false` if the range
    /// escapes RAM (unmapped, or a `Trap`/MMIO region) — the interpreter turns that
    /// into a fault, matching a scalar `read`. Used for f64/f80/fxsave loads whose
    /// width exceeds the 8-byte scalar path.
    pub fn read_ram_guest(&self, addr: u64, buf: &mut [u8]) -> bool {
        match self.region_for(addr, buf.len()) {
            Some(r) if matches!(r.kind, RegionKind::Ram) => {
                let start = addr as usize;
                // SAFETY: `region_for` bounds-checked the range into a mapped RAM
                // region, hence inside the backing buffer; read-only view.
                let backing = unsafe { &*self.backing.get() };
                buf.copy_from_slice(&backing[start..start + buf.len()]);
                true
            }
            _ => false,
        }
    }

    /// Guest-side wide write for the x87 helpers: copy `bytes` into a mapped **RAM**
    /// region and record the SMC `note_write` (§10) — so a self-modifying x87 store
    /// onto a code page invalidates, exactly like a scalar `Store`. `false` if the
    /// range escapes RAM. An x87 store into a `Trap` region can't be expressed as
    /// `Exit::MmioWrite` (its value exceeds 8 bytes), so it faults here rather than
    /// silently scribbling backing — MMIO for x87 stays deferred (§5.2, §10).
    pub fn write_ram_guest(&self, addr: u64, bytes: &[u8]) -> bool {
        match self.region_for(addr, bytes.len()) {
            Some(r) if matches!(r.kind, RegionKind::Ram) => {
                let start = addr as usize;
                // SAFETY: the one deliberate interior-mutable write (§8); the range is
                // bounds-checked into a mapped RAM region.
                let backing = unsafe { &mut *self.backing.get() };
                backing[start..start + bytes.len()].copy_from_slice(bytes);
                self.note_write(addr, bytes.len());
                true
            }
            _ => false,
        }
    }

    /// Read bytes back out (inspection / HLE reading guest structures). (§4.2)
    pub fn read_bytes(&self, guest_addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
        if self.region_for(guest_addr, buf.len()).is_none() {
            return Err(MemError::Unmapped);
        }
        let start = guest_addr as usize;
        // SAFETY: the range lies inside a mapped, bounds-checked region; this is a
        // host-side read with no concurrent guest store to the same bytes.
        let backing = unsafe { &*self.backing.get() };
        buf.copy_from_slice(&backing[start..start + buf.len()]);
        Ok(())
    }

    /// Drop a region's tag. In `Flat` the backing buffer is untouched — only the
    /// permission/kind tag goes away. Partial unmap isn't modeled in M0, so the
    /// `(guest_addr, size)` must match a mapped region exactly. (§4.1)
    pub fn unmap(&mut self, guest_addr: u64, size: usize) -> Result<(), MapError> {
        match self
            .regions
            .iter()
            .position(|r| r.start == guest_addr && r.size == size)
        {
            Some(pos) => {
                self.regions.remove(pos);
                Ok(())
            }
            None => Err(MapError::OutOfBounds),
        }
    }

    /// The region containing `[addr, addr + size)`, or `Unmapped` if the range
    /// escapes every mapped region. Shared by scalar read/write.
    fn region_at(&self, addr: u64, size: u8) -> Result<&Region, MemTrap> {
        let end = addr.checked_add(size as u64).ok_or(MemTrap::Unmapped)?;
        self.regions
            .iter()
            .find(|r| r.start <= addr && end <= r.start + r.size as u64)
            .ok_or(MemTrap::Unmapped)
    }

    /// Scalar read used by the interpreter and trap-out path (§8.1). Little-endian.
    /// MUST bounds-check (§8.2.3) — an out-of-range addr is a MemTrap, never a panic/UB.
    /// A `Trap` region yields `MemTrap::Mmio` (routed out as `Exit::MmioRead`).
    pub fn read(&self, addr: u64, size: u8) -> Result<u64, MemTrap> {
        let region = self.region_at(addr, size)?;
        if matches!(region.kind, RegionKind::Trap) {
            return Err(MemTrap::Mmio);
        }
        let start = addr as usize;
        // SAFETY: read-only view of the backing buffer; the range is bounds-checked
        // to lie inside a mapped RAM region (§8.2.3).
        let backing = unsafe { &*self.backing.get() };
        let mut buf = [0u8; 8];
        buf[..size as usize].copy_from_slice(&backing[start..start + size as usize]);
        Ok(u64::from_le_bytes(buf))
    }

    /// Contiguous code bytes for the iced `Decoder` (the lift, §7.3).
    /// Scalar `read` can't feed a decoder — it needs a byte slice. Returns up to
    /// `max_len` bytes from `addr`, capped at the containing region's end (a block
    /// that runs off the region simply re-lifts from the next one). `Unmapped` if
    /// `addr` isn't inside a mapped region.
    pub fn code_slice(&self, addr: u64, max_len: usize) -> Result<&[u8], MemTrap> {
        let region = self
            .regions
            .iter()
            .find(|r| r.start <= addr && addr < r.start + r.size as u64)
            .ok_or(MemTrap::Unmapped)?;
        let region_end = region.start + region.size as u64;
        let end = addr.saturating_add(max_len as u64).min(region_end);
        // SAFETY: read-only view; `[addr, end)` lies inside a mapped region, hence
        // inside the backing buffer. The borrow is tied to `&self`.
        let backing = unsafe { &*self.backing.get() };
        Ok(&backing[addr as usize..end as usize])
    }

    /// `&self`, not `&mut self` (§8 pitfall) — guest RAM is interior-mutable and shared.
    /// Little-endian. Bounds-checked; a `Trap` region yields `MemTrap::Mmio`.
    pub fn write(&self, addr: u64, val: u64, size: u8) -> Result<(), MemTrap> {
        let region = self.region_at(addr, size)?;
        if matches!(region.kind, RegionKind::Trap) {
            return Err(MemTrap::Mmio);
        }
        let start = addr as usize;
        let bytes = val.to_le_bytes();
        // SAFETY: the one deliberate interior-mutable write (§8). Guest stores race
        // like real hardware; ordering comes from TSO barriers, not `&mut`. The range
        // is bounds-checked to lie inside a mapped RAM region.
        let backing = unsafe { &mut *self.backing.get() };
        backing[start..start + size as usize].copy_from_slice(&bytes[..size as usize]);
        self.note_write(addr, size as usize); // SMC: catch a store onto a code page (§10)
        Ok(())
    }

    /// Atomic read-modify-write on a mapped RAM location (§8.2.3, §11). Returns the
    /// prior value (size-masked). Sequentially consistent — a locked op is a full
    /// sync point. A naturally-aligned access uses a real host atomic; a misaligned
    /// one (rare; x86 permits it via a bus lock) falls back to a plain RMW — the
    /// *value* is identical, only cross-thread atomicity is lost, which aligned
    /// guest atomics never hit.
    pub fn atomic_rmw(&self, addr: u64, src: u64, size: u8, op: RmwOp) -> Result<u64, MemTrap> {
        let region = self.region_at(addr, size)?;
        if matches!(region.kind, RegionKind::Trap) {
            return Err(MemTrap::Mmio);
        }
        // SAFETY: bounds-checked into a mapped RAM region; `ptr` is inside the
        // backing buffer. Interior-mutable shared access is the intended model (§8).
        let ptr = unsafe { (*self.backing.get()).as_mut_ptr().add(addr as usize) };
        let old = unsafe { atomic_rmw_raw(ptr, src, size, op) };
        self.note_write(addr, size as usize);
        Ok(old & mask_bits(size))
    }

    /// Atomic compare-exchange (`cmpxchg`, §8.2.3). If `[addr] == expected`, store
    /// `src`; return the prior value either way (size-masked).
    pub fn atomic_cas(&self, addr: u64, expected: u64, src: u64, size: u8) -> Result<u64, MemTrap> {
        let region = self.region_at(addr, size)?;
        if matches!(region.kind, RegionKind::Trap) {
            return Err(MemTrap::Mmio);
        }
        // SAFETY: as in `atomic_rmw`.
        let ptr = unsafe { (*self.backing.get()).as_mut_ptr().add(addr as usize) };
        let old = unsafe { atomic_cas_raw(ptr, expected & mask_bits(size), src, size) };
        self.note_write(addr, size as usize);
        Ok(old & mask_bits(size))
    }
}

fn mask_bits(size: u8) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

/// Raw atomic RMW dispatch over a guest pointer. Aligned → real host atomic;
/// misaligned → plain read/modify/write (see `Memory::atomic_rmw`).
///
/// # Safety
/// `ptr` must point to `size` valid, mapped bytes inside the backing buffer.
unsafe fn atomic_rmw_raw(ptr: *mut u8, src: u64, size: u8, op: RmwOp) -> u64 {
    use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering::SeqCst};

    macro_rules! rmw {
        ($atom:ty, $int:ty) => {{
            if ptr as usize % std::mem::size_of::<$int>() == 0 {
                let a = unsafe { &*(ptr as *const $atom) };
                let s = src as $int;
                let old = match op {
                    RmwOp::Add => a.fetch_add(s, SeqCst),
                    RmwOp::Sub => a.fetch_sub(s, SeqCst),
                    // No native reverse-subtract atomic: CAS loop (new = s - cur).
                    RmwOp::Rsub => {
                        let mut cur = a.load(SeqCst);
                        loop {
                            match a.compare_exchange_weak(cur, s.wrapping_sub(cur), SeqCst, SeqCst)
                            {
                                Ok(v) => break v,
                                Err(v) => cur = v,
                            }
                        }
                    }
                    RmwOp::And => a.fetch_and(s, SeqCst),
                    RmwOp::Or => a.fetch_or(s, SeqCst),
                    RmwOp::Xor => a.fetch_xor(s, SeqCst),
                    RmwOp::Xchg => a.swap(s, SeqCst),
                };
                old as u64
            } else {
                let old = unsafe { plain_read(ptr, size) };
                let new = apply_rmw(old, src, op, size);
                unsafe { plain_write(ptr, new, size) };
                old
            }
        }};
    }

    match size {
        1 => rmw!(AtomicU8, u8),
        2 => rmw!(AtomicU16, u16),
        4 => rmw!(AtomicU32, u32),
        _ => rmw!(AtomicU64, u64),
    }
}

/// # Safety
/// As `atomic_rmw_raw`.
unsafe fn atomic_cas_raw(ptr: *mut u8, expected: u64, src: u64, size: u8) -> u64 {
    use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering::SeqCst};

    macro_rules! cas {
        ($atom:ty, $int:ty) => {{
            if ptr as usize % std::mem::size_of::<$int>() == 0 {
                let a = unsafe { &*(ptr as *const $atom) };
                // Failure returns the current value; success returns `expected`.
                match a.compare_exchange(expected as $int, src as $int, SeqCst, SeqCst) {
                    Ok(v) => v as u64,
                    Err(v) => v as u64,
                }
            } else {
                let old = unsafe { plain_read(ptr, size) };
                if old == expected {
                    unsafe { plain_write(ptr, src, size) };
                }
                old
            }
        }};
    }

    match size {
        1 => cas!(AtomicU8, u8),
        2 => cas!(AtomicU16, u16),
        4 => cas!(AtomicU32, u32),
        _ => cas!(AtomicU64, u64),
    }
}

unsafe fn plain_read(ptr: *const u8, size: u8) -> u64 {
    let mut buf = [0u8; 8];
    unsafe { std::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), size as usize) };
    u64::from_le_bytes(buf)
}

unsafe fn plain_write(ptr: *mut u8, val: u64, size: u8) {
    let bytes = val.to_le_bytes();
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, size as usize) };
}

fn apply_rmw(old: u64, src: u64, op: RmwOp, size: u8) -> u64 {
    let m = mask_bits(size);
    let r = match op {
        RmwOp::Add => old.wrapping_add(src),
        RmwOp::Sub => old.wrapping_sub(src),
        RmwOp::Rsub => src.wrapping_sub(old),
        RmwOp::And => old & src,
        RmwOp::Or => old | src,
        RmwOp::Xor => old ^ src,
        RmwOp::Xchg => src,
    };
    r & m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(size: u64) -> Memory {
        Memory::new(MemoryModel::Flat { size })
    }

    #[test]
    fn map_within_bounds_ok() {
        let mut m = flat(0x1000);
        assert!(m.map(0x100, 0x200, Prot::RW, RegionKind::Ram).is_ok());
    }

    #[test]
    fn map_past_end_is_out_of_bounds() {
        let mut m = flat(0x1000);
        assert!(matches!(
            m.map(0xF00, 0x200, Prot::RW, RegionKind::Ram),
            Err(MapError::OutOfBounds)
        ));
    }

    #[test]
    fn map_overflowing_end_is_out_of_bounds() {
        let mut m = flat(0x1000);
        assert!(matches!(
            m.map(u64::MAX, 0x10, Prot::RW, RegionKind::Ram),
            Err(MapError::OutOfBounds)
        ));
    }

    #[test]
    fn overlapping_map_rejected() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x200, Prot::RW, RegionKind::Ram).unwrap();
        assert!(matches!(
            m.map(0x200, 0x100, Prot::RW, RegionKind::Ram),
            Err(MapError::Overlap)
        ));
    }

    #[test]
    fn adjacent_maps_allowed() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x100, Prot::RW, RegionKind::Ram).unwrap();
        assert!(m.map(0x200, 0x100, Prot::RW, RegionKind::Ram).is_ok());
    }

    #[test]
    fn write_then_read_roundtrips() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x200, Prot::RW, RegionKind::Ram).unwrap();
        m.write_bytes(0x110, &[1, 2, 3, 4]).unwrap();
        let mut buf = [0u8; 4];
        m.read_bytes(0x110, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn write_outside_mapped_region_is_unmapped() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x10, Prot::RW, RegionKind::Ram).unwrap();
        assert!(matches!(
            m.write_bytes(0x108, &[0; 0x10]),
            Err(MemError::Unmapped)
        ));
    }

    #[test]
    fn read_unmapped_is_unmapped() {
        let m = flat(0x1000);
        let mut buf = [0u8; 4];
        assert!(matches!(
            m.read_bytes(0x100, &mut buf),
            Err(MemError::Unmapped)
        ));
    }

    #[test]
    fn unmap_then_access_is_unmapped() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x200, Prot::RW, RegionKind::Ram).unwrap();
        m.unmap(0x100, 0x200).unwrap();
        let mut buf = [0u8; 4];
        assert!(matches!(
            m.read_bytes(0x110, &mut buf),
            Err(MemError::Unmapped)
        ));
    }

    #[test]
    fn unmap_must_match_a_region_exactly() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x200, Prot::RW, RegionKind::Ram).unwrap();
        assert!(matches!(m.unmap(0x100, 0x100), Err(MapError::OutOfBounds)));
    }

    #[test]
    fn unmap_frees_the_range_for_remapping() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x200, Prot::RW, RegionKind::Ram).unwrap();
        m.unmap(0x100, 0x200).unwrap();
        assert!(m.map(0x100, 0x200, Prot::RX, RegionKind::Ram).is_ok());
    }

    #[test]
    fn scalar_write_read_little_endian() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x100, Prot::RW, RegionKind::Ram).unwrap();
        m.write(0x110, 0x1122_3344_5566_7788, 8).unwrap();
        assert_eq!(m.read(0x110, 8).unwrap(), 0x1122_3344_5566_7788);
        // Sub-word reads see the low bytes (LE).
        assert_eq!(m.read(0x110, 1).unwrap(), 0x88);
        assert_eq!(m.read(0x110, 2).unwrap(), 0x7788);
        assert_eq!(m.read(0x110, 4).unwrap(), 0x5566_7788);
        // Individual bytes in memory, low byte first.
        let mut raw = [0u8; 8];
        m.read_bytes(0x110, &mut raw).unwrap();
        assert_eq!(raw, [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    }

    #[test]
    fn scalar_access_outside_region_traps_unmapped() {
        let m = flat(0x1000);
        assert!(matches!(m.read(0x10, 4), Err(MemTrap::Unmapped)));
        assert!(matches!(m.write(0x10, 0, 4), Err(MemTrap::Unmapped)));
    }

    #[test]
    fn scalar_access_straddling_region_end_traps() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x10, Prot::RW, RegionKind::Ram).unwrap();
        // Last valid 4-byte read starts at 0x10c; 0x10e straddles the end.
        assert!(m.read(0x10c, 4).is_ok());
        assert!(matches!(m.read(0x10e, 4), Err(MemTrap::Unmapped)));
    }

    #[test]
    fn trap_region_routes_to_mmio() {
        let mut m = flat(0x1000);
        m.map(0x200, 0x10, Prot::RW, RegionKind::Trap).unwrap();
        assert!(matches!(m.read(0x200, 4), Err(MemTrap::Mmio)));
        assert!(matches!(m.write(0x200, 0, 4), Err(MemTrap::Mmio)));
    }

    #[test]
    fn code_slice_caps_at_region_end() {
        let mut m = flat(0x1000);
        m.map(0x100, 0x8, Prot::RX, RegionKind::Ram).unwrap();
        m.write_bytes(0x100, &[0x90, 0x90, 0x90, 0xc3, 0, 0, 0, 0])
            .unwrap();
        // Asking for more than the region holds caps at its end (8 bytes).
        let s = m.code_slice(0x100, 64).unwrap();
        assert_eq!(s.len(), 8);
        assert_eq!(&s[..4], &[0x90, 0x90, 0x90, 0xc3]);
        // Mid-region start shortens accordingly.
        assert_eq!(m.code_slice(0x104, 64).unwrap().len(), 4);
        assert!(matches!(m.code_slice(0x50, 4), Err(MemTrap::Unmapped)));
    }
}
