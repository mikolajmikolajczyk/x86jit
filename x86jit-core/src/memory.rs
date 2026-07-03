//! Guest memory model (§4.1, §4.2, §8.1).

use std::cell::UnsafeCell;

/// Memory model selection. Start with `Flat`; add `SoftMmu` when the guest
/// uses a sparse, high address space (§4.1).
pub enum MemoryModel {
    /// One contiguous host buffer of `size` bytes representing guest space
    /// `[0, size)`. Translation is `host_base + guest_addr`. `map()` only
    /// tags regions; it does not allocate. Addresses `>= size` are unmapped.
    Flat { size: u64 },
    /// Sparse address space via a page/region table. `map()` allocates pages.
    SoftMmu,
}

/// Access protection for a mapped region (§4.2).
pub enum Prot {
    R,
    RW,
    RX,
    RWX,
}

/// Region behavior (§4.2).
pub enum RegionKind {
    /// Ordinary RAM. Access is inlined into generated code — no trap-out.
    Ram,
    /// Trapped region: every access yields `Exit::MmioRead`/`MmioWrite`.
    Trap,
}

/// Why a memory access could not complete inline (§8.1). The interpreter and
/// codegen turn this into the appropriate `Exit`.
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
pub struct Memory {
    // Read once SoftMmu translation lands; retained for the model switch (§4.1).
    #[allow(dead_code)]
    model: MemoryModel,
    // Flat backing store behind UnsafeCell so `write(&self)` is sound; SoftMmu
    // region table comes later. Access must be bounds-checked (§8.2.3) — no raw
    // out-of-range indexing, that would be host UB.
    backing: UnsafeCell<Box<[u8]>>,
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
        Self {
            model,
            backing: UnsafeCell::new(backing),
        }
    }

    /// Base pointer of the guest RAM buffer (JIT inlines `host_base + addr`).
    pub fn host_base(&self) -> *const u8 {
        // SAFETY: pointer to the backing buffer's start; callers bounds-check.
        unsafe { (*self.backing.get()).as_ptr() }
    }

    pub fn map(
        &mut self,
        _guest_addr: u64,
        _size: usize,
        _prot: Prot,
        _kind: RegionKind,
    ) -> Result<(), MapError> {
        todo!("M0: record region prot/kind (Flat) or allocate pages (SoftMmu)")
    }

    pub fn write_bytes(&mut self, _guest_addr: u64, _bytes: &[u8]) -> Result<(), MemError> {
        todo!("M0: copy bytes into backing store")
    }

    pub fn read_bytes(&self, _guest_addr: u64, _buf: &mut [u8]) -> Result<(), MemError> {
        todo!("M0: copy bytes out of backing store")
    }

    pub fn unmap(&mut self, _guest_addr: u64, _size: usize) -> Result<(), MapError> {
        todo!("M0: drop region")
    }

    /// Scalar read used by the interpreter and trap-out path (§8.1).
    /// MUST bounds-check (§8.2.3) — an out-of-range addr is a MemTrap, never a panic/UB.
    pub fn read(&self, _addr: u64, _size: u8) -> Result<u64, MemTrap> {
        todo!("M1: bounds-check, RAM read or MemTrap")
    }

    /// Contiguous code bytes for the iced `Decoder` (the lift, §7.3).
    /// Scalar `read` can't feed a decoder — it needs a byte slice. Returns up to
    /// `max_len` bytes from `addr` within the mapped region.
    /// Flat: a subslice of the backing buffer. SoftMmu: must not cross a page
    /// boundary silently — cap at the page end (a block that runs off a mapped
    /// page re-lifts from the next page). `Unmapped` if `addr` isn't executable.
    pub fn code_slice(&self, _addr: u64, _max_len: usize) -> Result<&[u8], MemTrap> {
        todo!("M1: return a bounded code slice for the decoder (§7.3)")
    }

    /// `&self`, not `&mut self` (§8 pitfall) — guest RAM is interior-mutable and shared.
    pub fn write(&self, _addr: u64, _val: u64, _size: u8) -> Result<(), MemTrap> {
        todo!("M1: bounds-check via UnsafeCell, RAM write or MemTrap")
    }
}
