//! Guest memory model (§4.1, §4.2, §8.1).

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::ir::RmwOp;

/// Memory model selection. Start with `Flat`; use `Reserved` when the guest wants a
/// sparse, huge address space (Go's ~768 GiB arena hints) (§4.1).
#[derive(Clone, Copy, Debug)]
pub enum MemoryModel {
    /// One contiguous host buffer of `size` bytes representing guest space
    /// `[0, size)`. Translation is `host_base + guest_addr`. `map()` only
    /// tags regions; it does not allocate. Addresses `>= size` are unmapped.
    Flat { size: u64 },
    /// A huge (`span`-byte) guest address space `[0, span)`, same one-add translation
    /// as `Flat` but sparsely committed: untouched guest VA (a `PROT_NONE`
    /// reservation, Go's 768 GiB arena hints) costs no physical memory. To actually
    /// reserve hundreds of GiB on a normal host the backing must be a `MAP_NORESERVE`
    /// mapping — which the guest-agnostic core can't allocate (no OS dep), so the
    /// embedder provides it via [`Memory::from_host_ram`]. Constructing this model
    /// through [`Memory::new`] instead uses a plain `Vec` (fine only for a modest
    /// span or a test). Forecloses per-page guest protections (ADR-0001,
    /// go-caddy-plan.md Phase 1).
    Reserved { span: u64 },
    /// Sparse address space via a page/region table. `map()` allocates pages.
    SoftMmu,
}

/// Host-provided backing for a `Reserved` address space (ADR-0001). The
/// guest-agnostic core must not depend on an OS allocator, so an embedder that
/// needs a huge sparse mapping (a `MAP_NORESERVE` mmap for Go's 768 GiB arena hints)
/// allocates it host-side and hands core the raw region through
/// [`Memory::from_host_ram`]. `dtor(ptr, len)` frees it when the `Memory` drops.
///
/// # Safety
/// `ptr` must be valid for reads and writes over `[ptr, ptr+len)` for the lifetime
/// of the `Memory` it backs, and `dtor` must correctly release exactly that region.
/// Guard-page hook: `protect(page_ptr, len, accessible)` `mprotect`s a page-aligned
/// sub-range of a [`HostRam`] mapping RW (`true`) or `PROT_NONE` (`false`) (doc-30 GP-1).
pub type ProtectFn = Box<dyn Fn(*mut u8, usize, bool) + Send + Sync>;

pub struct HostRam {
    pub ptr: *mut u8,
    pub len: usize,
    pub dtor: Box<dyn FnMut(*mut u8, usize) + Send>,
    /// Optional guard-page hook (doc-30 GP-1): `protect(page_ptr, len, accessible)`
    /// flips a page-aligned sub-range of this mapping between accessible (`true` →
    /// `PROT_READ|PROT_WRITE`) and inaccessible (`false` → `PROT_NONE`). `Memory::map`/
    /// `unmap` call it so an in-span-but-unmapped access hardware-faults, matching the
    /// interpreter's `region_at` trap. `None` (the default) → no guard pages, the
    /// pre-GP-1 behavior. Only a host mapping can be `mprotect`ed — a `Vec` backing
    /// leaves this `None`.
    pub protect: Option<ProtectFn>,
}

impl Drop for HostRam {
    fn drop(&mut self) {
        (self.dtor)(self.ptr, self.len);
    }
}

/// Keeps the backing allocation alive for the `Memory`'s lifetime. `Backing` reads
/// bytes through a raw `ptr`/`len` (uniform hot path); this only owns the storage.
enum Owner {
    Boxed(#[allow(dead_code)] Box<[u8]>),
    Host(#[allow(dead_code)] HostRam),
}

/// The contiguous host byte range backing guest RAM, translated by
/// `host_base + guest_addr`. `ptr`/`len` are the access path (identical for an
/// owned `Box` or a host-provided mapping); `owner` keeps the storage alive.
struct Backing {
    ptr: *mut u8,
    len: usize,
    owner: Owner,
}

// SAFETY: the raw pointer names memory this `Backing` exclusively owns (freed only
// via `owner` on drop). The bytes are guest RAM, raced across vcpus exactly like the
// old `Box<[u8]>` — ordering comes from emitted TSO barriers, not Rust aliasing (§8).
unsafe impl Send for Backing {}
unsafe impl Sync for Backing {}
unsafe impl Send for HostRam {}
unsafe impl Sync for HostRam {}

impl Backing {
    /// Own a heap `Box<[u8]>` (the `Flat`/`Reserved`-via-`Vec` path).
    fn boxed(mut b: Box<[u8]>) -> Backing {
        let ptr = b.as_mut_ptr();
        let len = b.len();
        Backing {
            ptr,
            len,
            owner: Owner::Boxed(b),
        }
    }

    /// Adopt an embedder-provided host mapping (the `Reserved` NORESERVE path).
    fn host(ram: HostRam) -> Backing {
        let (ptr, len) = (ram.ptr, ram.len);
        Backing {
            ptr,
            len,
            owner: Owner::Host(ram),
        }
    }

    fn len(&self) -> usize {
        self.len
    }
    fn as_ptr(&self) -> *const u8 {
        self.ptr
    }
    /// # Safety: interior-mutability discipline (§8) — concurrent guest stores race.
    unsafe fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
    /// # Safety: as [`Backing::as_slice`].
    #[allow(clippy::mut_from_ref)]
    unsafe fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// Flip host protection on `[ptr+off, ptr+off+len)` via the embedder's guard-page
    /// hook, if this is a host mapping that installed one (doc-30 GP-1). A no-op for a
    /// `Vec` backing or a host mapping with no hook.
    fn protect(&self, off: usize, len: usize, accessible: bool) {
        if let Owner::Host(ram) = &self.owner {
            if let Some(f) = &ram.protect {
                // SAFETY: `[off, off+len)` is caller-clamped inside `[0, self.len)`; the
                // embedder `mprotect`s a page-aligned sub-range of the mapping it owns.
                f(unsafe { self.ptr.add(off) }, len, accessible);
            }
        }
    }
}

/// Host page size assumed for guard-page `mprotect` rounding (doc-30 GP-1). 4 KiB on
/// every host this project targets (x86-64 and the 4 KiB aarch64 CI). A 16 KiB-page
/// host would need this parameterized — recorded as a limitation, not a config we run.
const HOST_PAGE: u64 = 4096;

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

/// Upper bound on the guest address range the SMC code-page table tracks. `Flat`
/// tracks its whole (small) backing; a `Reserved` span is up to 1 TiB, and one
/// `AtomicBool` per 4 KiB page across all of it would itself commit hundreds of MiB
/// — defeating the sparse backing. Guest code always lives in the low image/interp
/// region, never in the multi-hundred-GiB heap it reserves, so tracking only the
/// low `CODE_WINDOW` is correct: `mark_code`/`note_write` for a page beyond the
/// table simply no-op (`code_page.get` returns `None`), and no code ever executes
/// from there. 4 GiB comfortably covers any ELF image plus its interpreter.
const CODE_WINDOW: u64 = 4 << 30;

pub struct Memory {
    // Selects the mapping strategy in `map()`; retained for the SoftMmu switch (§4.1).
    model: MemoryModel,
    // Flat backing store behind UnsafeCell so `write(&self)` is sound; SoftMmu
    // region table comes later. Access must be bounds-checked (§8.2.3) — no raw
    // out-of-range indexing, that would be host UB.
    backing: UnsafeCell<Backing>,
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
    // Last region index `region_at` hit — a locality cache to skip the linear scan on
    // the common case (consecutive accesses to the same region). Interior-mutable
    // (`region_at` is `&self`); only ever a hint, so `Relaxed` and staleness are fine.
    last_region: AtomicUsize,
}

// SAFETY: concurrent guest stores are intended to race like real hardware; the
// guest's expected ordering is provided by emitted TSO barriers, not by Rust
// aliasing rules. No host-side invariant is broken by concurrent access to the
// flat byte buffer (bounds-checked per access).
unsafe impl Sync for Memory {}

impl Memory {
    pub fn new(model: MemoryModel) -> Self {
        let bytes: Box<[u8]> = match &model {
            // A `Reserved` model built through `new` uses a plain `Vec` (fine for a
            // modest span or a test); a huge NORESERVE span comes in via
            // `from_host_ram`, where the embedder owns the mapping.
            MemoryModel::Flat { size } | MemoryModel::Reserved { span: size } => {
                vec![0u8; *size as usize].into_boxed_slice()
            }
            MemoryModel::SoftMmu => Box::new([]),
        };
        Self::from_backing(model, Backing::boxed(bytes))
    }

    /// Build a `Reserved` memory over an embedder-provided host mapping (ADR-0001).
    /// Core can't allocate a `MAP_NORESERVE` span itself (it must stay guest-agnostic,
    /// depending only on the decoder), so the embedder hands in the raw region; the
    /// `HostRam` dtor frees it on drop. `ram.len` is the span the JIT bounds against.
    pub fn from_host_ram(model: MemoryModel, ram: HostRam) -> Self {
        // The JIT and `map()` bound guest accesses against the model span (`size()`),
        // but the backing is only `ram.len` bytes. If the span exceeded the mapping, an
        // in-span access past `ram.len` would pass the bound and dereference past the
        // host mapping (OOB/UB in the JIT, slice panic in the interpreter). Require the
        // span to fit the backing so the two can't diverge.
        let span = match &model {
            MemoryModel::Flat { size } | MemoryModel::Reserved { span: size } => *size as usize,
            MemoryModel::SoftMmu => 0,
        };
        assert!(
            span <= ram.len,
            "host RAM backing ({} bytes) is smaller than the model span ({} bytes)",
            ram.len,
            span,
        );
        Self::from_backing(model, Backing::host(ram))
    }

    fn from_backing(model: MemoryModel, backing: Backing) -> Self {
        // Note on atomics: a guest LOCK atomic runs as a host atomic only when the host
        // pointer (`base + guest_addr`) is naturally aligned (see `atomic_rmw_raw`); a
        // guest-aligned atomic over a host-misaligned base degrades to a non-atomic RMW.
        // The system allocator and `mmap` return ≥16-aligned storage in practice, so host
        // alignment tracks guest alignment — but that is not a type-level guarantee
        // (`[u8]`'s layout is align-1, honored literally by Miri or a swapped
        // `#[global_allocator]`), so it is documented here rather than asserted (an assert
        // would false-abort a legitimate run on such an allocator).
        let code_page = fresh_code_pages(backing.len());
        Self {
            model,
            backing: UnsafeCell::new(backing),
            regions: Vec::new(),
            code_page,
            dirty: Mutex::new(Vec::new()),
            dirty_flag: AtomicBool::new(false),
            last_region: AtomicUsize::new(0),
        }
    }

    /// Deep-copy this memory into an independent `Memory` with identical contents
    /// and region tags but its own (empty) SMC/dirty state — the guest-agnostic
    /// primitive behind `fork` (§4.2). The child's byte buffer is a fresh
    /// allocation; writes on either side don't affect the other.
    pub fn deep_copy(&self) -> Option<Memory> {
        // SAFETY: snapshot at a quiescent point (between guest steps); no concurrent
        // vcpu writes the parent during a fork.
        let src = unsafe { (*self.backing.get()).as_slice() };
        let owner = &unsafe { &*self.backing.get() }.owner;
        let bytes: Box<[u8]> = match (self.model, owner) {
            // A host-backed (NORESERVE) `Reserved` span can't be re-allocated by the
            // core (that's the embedder's job), and cloning it into a `Vec` would
            // commit the whole span. Fork of such a memory is unsupported — `None` lets
            // the embedder surface a typed error instead of aborting the host (was a
            // `panic!`). Go, the only huge-`Reserved` guest, never forks; forking
            // guests use `Flat`.
            (MemoryModel::Reserved { .. }, Owner::Host(_)) => return None,
            // A host-backed guarded `Flat` (GP-5, x86jit-run's non-Go path) or an owned
            // `Reserved` (Vec-backed, modest span): copy only tagged regions into a fresh
            // demand-zero span. For the guarded host case this is also *required* — the
            // unmapped holes are `PROT_NONE`, so a whole-span `to_vec` would fault. The
            // child is `Vec`-backed (no guards — the documented residual; a forking guest
            // typically execve's immediately, which reloads a fresh guarded span).
            (MemoryModel::Reserved { span }, Owner::Boxed(_))
            | (MemoryModel::Flat { size: span }, Owner::Host(_)) => {
                let mut child = vec![0u8; span as usize].into_boxed_slice();
                for r in &self.regions {
                    let s = r.start as usize;
                    let e = s + r.size;
                    child[s..e].copy_from_slice(&src[s..e]);
                }
                child
            }
            _ => src.to_vec().into_boxed_slice(),
        };
        Some(
            Memory::from_backing(self.model, Backing::boxed(bytes))
                .with_regions(self.regions.clone()),
        )
    }

    fn with_regions(mut self, regions: Vec<Region>) -> Self {
        self.regions = regions;
        self
    }

    /// Tag every page spanned by `[addr, addr+len)` as backing translated code
    /// (§10). Called through `&self` when a block is cached, so a later store to
    /// the page is caught. Idempotent.
    pub fn mark_code(&self, addr: u64, len: u32) {
        let last = addr.saturating_add(len.max(1) as u64 - 1);
        // A page beyond the low `CODE_WINDOW` table simply no-ops (`code_page.get` →
        // None), the documented graceful degradation above — a store to code placed
        // above the window would not be tracked. Not asserted: the corpus keeps code
        // low, but a >4 GiB Flat, or a block straddling the window edge, is a valid
        // configuration that must not abort.
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

    /// Clear every code-page tag — for a whole-cache invalidation (e.g. mapping a
    /// Trap region, §5.2 M4-T10), the bulk counterpart of [`Self::clear_code_page`].
    pub fn clear_all_code_pages(&self) {
        for bit in self.code_page.iter() {
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
        // Common case (nothing self-modified): a shared `load`, not a `swap` — the
        // latter takes the cache line exclusive on every dispatch even when clean.
        if !self.dirty_flag.load(Ordering::Relaxed) {
            return Vec::new();
        }
        // Something's dirty: claim it. A racing vcpu may have drained it already.
        if !self.dirty_flag.swap(false, Ordering::Relaxed) {
            return Vec::new();
        }
        std::mem::take(&mut *self.dirty.lock().unwrap())
    }

    /// Highest mapped guest address (exclusive end of the top region) at or below
    /// `limit`, or 0 if nothing is mapped there. Lets an embedder place the heap just
    /// above a loaded image's segments instead of at a fixed guess (#14). A region that
    /// *straddles* `limit` (starts below it but extends past) has its end **clamped to
    /// `limit`** — otherwise the result would exceed `limit` and the caller would place
    /// the heap past the boundary it asked to stay under.
    pub fn highest_mapped_below(&self, limit: u64) -> u64 {
        self.regions
            .iter()
            .filter(|r| r.start < limit)
            .map(|r| (r.start + r.size as u64).min(limit))
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

    /// Size of the backing buffer in bytes — the bound the JIT checks a guest
    /// address against before an inlined access (§8.2.3). The buffer is allocated
    /// from this value, so it equals `backing.len()`. For `Reserved` it is the full
    /// (sparse) span, so the one-add translation reaches the whole reserved range.
    pub fn size(&self) -> u64 {
        match self.model {
            MemoryModel::Flat { size } => size,
            MemoryModel::Reserved { span } => span,
            MemoryModel::SoftMmu => 0,
        }
    }

    /// The `[lo, hi)` guest-address span enclosing every `Trap` (MMIO) region, or
    /// `None` if there are none (§5.2, M4-T10). The JIT bakes this window as a
    /// compile-time constant and, when present, adds a range check that defers an
    /// inlined access to the interpreter. It is the *bounding* window: an address in
    /// a RAM gap between two Trap regions is conservatively deferred too — correct
    /// (the interpreter handles RAM), only slightly slower. `None` means zero
    /// per-access cost, the common case.
    pub fn trap_window(&self) -> Option<(u64, u64)> {
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        for r in &self.regions {
            if matches!(r.kind, RegionKind::Trap) {
                lo = lo.min(r.start);
                hi = hi.max(r.start.saturating_add(r.size as u64));
            }
        }
        (lo < hi).then_some((lo, hi))
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
            // `Reserved` tags and bounds-checks exactly like `Flat`; it only differs
            // in how the backing bytes are allocated (a sparse mmap vs a `Vec`).
            MemoryModel::Flat { size: total } | MemoryModel::Reserved { span: total } => {
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
                // Guard pages (doc-30 GP-1): open the region's host pages. No-op unless
                // the backing installed a protect hook.
                self.reprotect(guest_addr, size, true);
                Ok(())
            }
            MemoryModel::SoftMmu => todo!("SoftMmu: allocate pages for the region (§4.1)"),
        }
    }

    /// Load bytes into an already-mapped region (e.g. an ELF segment). Host-side
    /// loader path: it bypasses guest `Prot` (you write code into an RX region),
    /// so it only checks that the range is mapped. (§4.2)
    pub fn write_bytes(&self, guest_addr: u64, bytes: &[u8]) -> Result<(), MemError> {
        if self.region_for(guest_addr, bytes.len()).is_none() {
            return Err(MemError::Unmapped);
        }
        let start = guest_addr as usize;
        // `&self`, interior-mutable (§8), mirroring `write`/`write_ram_guest`: the guest
        // memory model is shared-mutable so the syscall shim can write results through an
        // `Arc<Vm>` on a worker thread (M7 threaded embedder). The range sits inside a
        // mapped region `map()` bounds-checked.
        // SAFETY: the range is bounds-checked into a mapped region of the backing.
        let backing = unsafe { (*self.backing.get()).as_mut_slice() };
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
                let backing = unsafe { (*self.backing.get()).as_slice() };
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
                let backing = unsafe { (*self.backing.get()).as_mut_slice() };
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
        let backing = unsafe { (*self.backing.get()).as_slice() };
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
                // Guard pages (doc-30 GP-1): close the region's host pages, except any
                // page still touched by a surviving region.
                self.reprotect(guest_addr, size, false);
                Ok(())
            }
            None => Err(MapError::OutOfBounds),
        }
    }

    /// Flip host guard-page protection for the pages `[start, start+size)` touches
    /// (doc-30 GP-1). On map (`accessible = true`) the region's pages, **rounded
    /// outward**, become `PROT_READ|PROT_WRITE`. On unmap (`false`) they become
    /// `PROT_NONE`, **except** a boundary page still overlapped by a surviving region
    /// (a page shared with a live neighbor stays accessible). A no-op unless the
    /// backing is a host mapping with a protect hook. Ranges are clamped to the backing.
    fn reprotect(&self, start: u64, size: usize, accessible: bool) {
        let backing = unsafe { &*self.backing.get() };
        let cap = backing.len() as u64;
        let end = start.saturating_add(size as u64).min(cap);
        let lo = (start / HOST_PAGE) * HOST_PAGE;
        let hi = end.div_ceil(HOST_PAGE) * HOST_PAGE;
        if lo >= hi {
            return;
        }
        if accessible {
            backing.protect(lo as usize, (hi.min(cap) - lo) as usize, true);
            return;
        }
        // Inaccessible: page by page, skipping any page a surviving region overlaps.
        let mut page = lo;
        while page < hi {
            let pend = (page + HOST_PAGE).min(cap);
            let shared = self
                .regions
                .iter()
                .any(|r| r.start < pend && page < r.start + r.size as u64);
            if !shared && page < pend {
                backing.protect(page as usize, (pend - page) as usize, false);
            }
            page += HOST_PAGE;
        }
    }

    /// The region containing `[addr, addr + size)`, or `Unmapped` if the range
    /// escapes every mapped region. Shared by scalar read/write.
    fn region_at(&self, addr: u64, size: u8) -> Result<&Region, MemTrap> {
        let end = addr.checked_add(size as u64).ok_or(MemTrap::Unmapped)?;
        let contains = |r: &Region| r.start <= addr && end <= r.start + r.size as u64;
        // Fast path: accesses are highly local, so the last region we hit usually
        // still contains this one — check it before the linear scan. The index stays
        // valid because `regions` only mutates through `&mut self` (map/unmap), never
        // during `&self` execution; a stale index just misses the fast path.
        let hint = self.last_region.load(Ordering::Relaxed);
        if let Some(r) = self.regions.get(hint) {
            if contains(r) {
                return Ok(r);
            }
        }
        match self.regions.iter().position(contains) {
            Some(i) => {
                self.last_region.store(i, Ordering::Relaxed);
                Ok(&self.regions[i])
            }
            None => Err(MemTrap::Unmapped),
        }
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
        let backing = unsafe { (*self.backing.get()).as_slice() };
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
        let backing = unsafe { (*self.backing.get()).as_slice() };
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
        let backing = unsafe { (*self.backing.get()).as_mut_slice() };
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
        let ptr = unsafe { ((*self.backing.get()).as_ptr() as *mut u8).add(addr as usize) };
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
        let ptr = unsafe { ((*self.backing.get()).as_ptr() as *mut u8).add(addr as usize) };
        let old = unsafe { atomic_cas_raw(ptr, expected & mask_bits(size), src, size) };
        self.note_write(addr, size as usize);
        Ok(old & mask_bits(size))
    }
}

/// A fresh SMC code-page table for a backing of `backing_len` bytes, bounded to the
/// low `CODE_WINDOW` so a huge `Reserved` span doesn't commit a giant bool array
/// (guest code never lives in the reserved heap — see [`CODE_WINDOW`]).
fn fresh_code_pages(backing_len: usize) -> Box<[AtomicBool]> {
    let tracked = backing_len.min(CODE_WINDOW as usize);
    let pages = tracked.div_ceil(CODE_PAGE_SIZE as usize);
    (0..pages).map(|_| AtomicBool::new(false)).collect()
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

    fn reserved(span: u64) -> Memory {
        Memory::new(MemoryModel::Reserved { span })
    }

    /// A `Reserved` `Memory` over a caller-owned buffer, exercising the embedder
    /// (`from_host_ram`) path without a real host mmap: leak a `Box` and hand its
    /// raw region in with a dtor that reclaims it. (The huge-span NORESERVE sparsity
    /// proof lives in the embedder crate, where the mmap does.)
    fn reserved_host(span: usize) -> Memory {
        let buf = vec![0u8; span].into_boxed_slice();
        let ptr = Box::into_raw(buf) as *mut u8;
        let ram = HostRam {
            ptr,
            len: span,
            dtor: Box::new(|p, l| {
                // SAFETY: reconstruct exactly the boxed slice we leaked, then drop it.
                let slice = unsafe { std::slice::from_raw_parts_mut(p, l) };
                drop(unsafe { Box::from_raw(slice as *mut [u8]) });
            }),
            protect: None,
        };
        Memory::from_host_ram(MemoryModel::Reserved { span: span as u64 }, ram)
    }

    #[test]
    fn highest_mapped_below_clamps_a_straddling_region() {
        let mut m = flat(0x10000);
        assert_eq!(m.highest_mapped_below(0x8000), 0, "nothing mapped → 0");
        // A region wholly below the limit contributes its exclusive end.
        m.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap(); // [0x1000, 0x2000)
        assert_eq!(m.highest_mapped_below(0x8000), 0x2000);
        // A region that STRADDLES the limit (starts below, ends above) must not push
        // the result past the limit — it is clamped to the limit (the bug: it returned
        // start+size = 0x6000 > limit).
        m.map(0x3000, 0x3000, Prot::RW, RegionKind::Ram).unwrap(); // [0x3000, 0x6000)
        assert_eq!(
            m.highest_mapped_below(0x5000),
            0x5000,
            "a straddling region clamps to the limit, never above it"
        );
        // A region starting at/above the limit is excluded entirely.
        assert_eq!(m.highest_mapped_below(0x3000), 0x2000);
    }

    /// Recorded `(guest_page_off, len, accessible)` guard-page calls.
    type ProtectLog = std::sync::Arc<std::sync::Mutex<Vec<(u64, usize, bool)>>>;

    /// A host-backed `Memory` whose `protect` hook records `(guest_page_off, len,
    /// accessible)` — for testing the guard-page rounding (doc-30 GP-1) without a real
    /// `mprotect`. Leaks a `Box`, reclaimed by the dtor on drop.
    fn recording_host(span: usize) -> (Memory, ProtectLog) {
        use std::sync::{Arc, Mutex};
        let buf = vec![0u8; span].into_boxed_slice();
        let base = Box::into_raw(buf) as *mut u8;
        let base_addr = base as usize;
        let log: Arc<Mutex<Vec<(u64, usize, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = Arc::clone(&log);
        let ram = HostRam {
            ptr: base,
            len: span,
            dtor: Box::new(|p, l| {
                let slice = unsafe { std::slice::from_raw_parts_mut(p, l) };
                drop(unsafe { Box::from_raw(slice as *mut [u8]) });
            }),
            protect: Some(Box::new(move |page_ptr, len, accessible| {
                rec.lock()
                    .unwrap()
                    .push(((page_ptr as usize - base_addr) as u64, len, accessible));
            })),
        };
        (
            Memory::from_host_ram(MemoryModel::Reserved { span: span as u64 }, ram),
            log,
        )
    }

    #[test]
    fn guard_pages_map_opens_and_unmap_closes_region_pages() {
        let (mut m, log) = recording_host(0x10000);
        // A page-aligned region → exactly its one page opened.
        m.map(0x2000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        m.unmap(0x2000, 0x1000).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![(0x2000, 0x1000, true), (0x2000, 0x1000, false)]
        );
    }

    #[test]
    fn guard_pages_shared_edge_page_stays_open_until_last_region_unmaps() {
        let (mut m, log) = recording_host(0x10000);
        // Two sub-page regions sharing host page 0x2000.
        m.map(0x2000, 0x400, Prot::RW, RegionKind::Ram).unwrap(); // [0x2000,0x2400)
        m.map(0x2400, 0x400, Prot::RW, RegionKind::Ram).unwrap(); // [0x2400,0x2800)
        log.lock().unwrap().clear();
        // Unmapping the first must NOT close page 0x2000 — the second still lives there.
        m.unmap(0x2000, 0x400).unwrap();
        assert!(
            log.lock().unwrap().is_empty(),
            "a page shared with a surviving region must stay accessible"
        );
        // Unmapping the last closes it.
        m.unmap(0x2400, 0x400).unwrap();
        assert_eq!(*log.lock().unwrap(), vec![(0x2000, 0x1000, false)]);
    }

    #[test]
    fn reserved_maps_and_roundtrips_like_flat() {
        let mut m = reserved(1 << 30); // 1 GiB span
        m.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
        m.write(0x1100, 0x1122_3344_5566_7788, 8).unwrap();
        assert_eq!(m.read(0x1100, 8).unwrap(), 0x1122_3344_5566_7788);
        assert!(matches!(m.read(0x50, 4), Err(MemTrap::Unmapped))); // unmapped gap
    }

    #[test]
    fn from_host_ram_backs_reserved() {
        // The embedder-backing path: map, write, read on host-provided RAM, and the
        // dtor runs on drop (no leak — miri/leak-checker would catch a bad dtor).
        let mut m = reserved_host(0x10000);
        m.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
        m.write(0x1040, 0xfeed_face, 8).unwrap();
        assert_eq!(m.read(0x1040, 8).unwrap(), 0xfeed_face);
        assert!(matches!(m.read(0x40, 4), Err(MemTrap::Unmapped)));
    }

    #[test]
    fn reserved_deep_copy_is_independent() {
        let mut parent = reserved(1 << 30);
        parent
            .map(0x1000, 0x1000, Prot::RW, RegionKind::Ram)
            .unwrap();
        parent.write(0x1000, 0xaa, 8).unwrap();
        let child = parent.deep_copy().expect("owned Reserved is deep-copyable");
        // Child sees the copied bytes...
        assert_eq!(child.read(0x1000, 8).unwrap(), 0xaa);
        // ...but writes don't cross over.
        parent.write(0x1000, 0xbb, 8).unwrap();
        assert_eq!(parent.read(0x1000, 8).unwrap(), 0xbb);
        assert_eq!(child.read(0x1000, 8).unwrap(), 0xaa);
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
