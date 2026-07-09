//! Host memory provider for the core's `Reserved` address space (ADR-0001).
//!
//! Go's runtime reserves a huge, sparse virtual space at startup (a ~600 MiB
//! `PROT_NONE` page-summary reservation plus heap-arena hints at 768 GiB). Backing
//! that needs a `MAP_NORESERVE` mapping — the kernel commits a physical page only on
//! first touch — which the guest-agnostic core deliberately can't allocate itself
//! (its sole dependency is the x86 decoder; see the boundary tripwire). So the OS
//! embedder mints the mapping here and hands it to `Memory::from_host_ram` as a
//! [`HostRam`] (go-caddy-plan.md Phase 1).

use x86jit_core::HostRam;

/// Reserve `span` bytes of sparse, lazily-committed host memory to back a `Reserved`
/// VM: `mmap(NULL, span, RW, PRIVATE|ANON|NORESERVE)`. Untouched guest VA costs no
/// physical memory; the returned dtor `munmap`s the whole span when the `Memory`
/// drops.
///
/// Panics if the host refuses the mapping (a strict-overcommit kernel, or a small-VA
/// host asked for more than it can address) — `span` is an embedder choice, so a
/// failure is a configuration error, not guest input.
pub fn reserve(span: u64) -> HostRam {
    let len = span as usize;
    assert!(len > 0, "Reserved span must be non-zero");
    // SAFETY: a standard anonymous mmap; fd -1, offset 0. NORESERVE leaves untouched
    // pages uncommitted. The result is checked against MAP_FAILED.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    assert!(
        ptr != libc::MAP_FAILED,
        "mmap({len} bytes, NORESERVE) failed: {}",
        std::io::Error::last_os_error()
    );
    HostRam {
        ptr: ptr as *mut u8,
        len,
        guest_base: 0,
        dtor: munmap_dtor(),
        protect: None,
    }
}

/// Reserve a sparse span at a **fixed** host address equal to `guest_base`, backing a
/// `Reserved` VM whose guest space is `[guest_base, span)` with **host == guest identity
/// mapping**: `ptr as u64 - guest_base == 0`, so the numeric host base is 0 and a guest
/// address equals its own host address. The embedder can then dereference a raw guest
/// pointer directly (`*(guest_addr as *const u8)`) and see the same byte the guest sees
/// — the property PS4-HLE syscall/GPU code relies on.
///
/// `mmap(guest_base, span - guest_base, RW, PRIVATE|ANON|NORESERVE|MAP_FIXED_NOREPLACE)`.
/// The low `[0, guest_base)` hole is never mapped (a null-adjacent mapping is UB to
/// reserve and pointless — `mmap_min_addr`), so the VM rejects any guest access below
/// `guest_base`.
///
/// `MAP_FIXED_NOREPLACE` places the mapping at exactly `guest_base` **without**
/// clobbering an existing mapping — it fails loudly (returns `MAP_FAILED`, never a
/// different address) if the range is taken, so a layout collision is caught at boot.
///
/// Pass `span` as the exclusive top guest address (e.g. 64 GiB) and `guest_base` as the
/// low cutoff (e.g. `0x10000`). Returns a [`HostRam`] carrying `guest_base`; construct
/// the VM with `MemoryModel::Reserved { span }` and `Vm::with_backend_host_ram`.
///
/// Panics if `guest_base >= span`, if `guest_base` isn't page-aligned, or if the host
/// refuses the fixed mapping (a layout collision or a strict-overcommit kernel) — each
/// is an embedder configuration error, not guest input.
pub fn reserve_at(guest_base: u64, span: u64) -> HostRam {
    assert!(
        guest_base < span,
        "guest_base (0x{guest_base:x}) must be below the span top (0x{span:x})"
    );
    assert!(
        guest_base % 4096 == 0,
        "guest_base (0x{guest_base:x}) must be page-aligned"
    );
    let len = (span - guest_base) as usize;
    // SAFETY: anonymous fixed mmap at `guest_base`; fd -1, offset 0. NORESERVE leaves
    // untouched pages uncommitted. MAP_FIXED_NOREPLACE fails (MAP_FAILED) rather than
    // relocating or clobbering if the range is already mapped. Checked below.
    let ptr = unsafe {
        libc::mmap(
            guest_base as *mut libc::c_void,
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE
                | libc::MAP_ANONYMOUS
                | libc::MAP_NORESERVE
                | libc::MAP_FIXED_NOREPLACE,
            -1,
            0,
        )
    };
    assert!(
        ptr != libc::MAP_FAILED,
        "mmap(0x{guest_base:x}, {len} bytes, FIXED_NOREPLACE|NORESERVE) failed: {}",
        std::io::Error::last_os_error()
    );
    // MAP_FIXED_NOREPLACE must honor the requested address exactly (an old kernel
    // lacking the flag could fall back to a hint and relocate — reject that so the
    // identity invariant can't silently break).
    assert_eq!(
        ptr as u64, guest_base,
        "MAP_FIXED_NOREPLACE returned 0x{:x}, not the requested guest_base 0x{guest_base:x}",
        ptr as u64
    );
    HostRam {
        ptr: ptr as *mut u8,
        len,
        guest_base,
        dtor: munmap_dtor(),
        protect: None,
    }
}

/// Like [`reserve`], but the span starts **`PROT_NONE`** and installs a guard-page
/// hook (doc-30 GP-1): `Memory::map` `mprotect`s a region's pages to `RW`, `unmap`
/// closes them again. An in-span-but-unmapped guest access then hardware-faults
/// (SIGSEGV) instead of silently reading demand-zero — the JIT gains the fault the
/// interpreter already produces (closes decision-3, once GP-2's handler converts the
/// signal to `Exit::UnmappedMemory`). Still `NORESERVE`-sparse.
pub fn reserve_guarded(span: u64) -> HostRam {
    let len = span as usize;
    assert!(len > 0, "Reserved span must be non-zero");
    // SAFETY: anonymous mmap, PROT_NONE so every page faults until a region opens it.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    assert!(
        ptr != libc::MAP_FAILED,
        "mmap({len} bytes, PROT_NONE NORESERVE) failed: {}",
        std::io::Error::last_os_error()
    );
    HostRam {
        ptr: ptr as *mut u8,
        len,
        guest_base: 0,
        dtor: munmap_dtor(),
        protect: Some(Box::new(|page_ptr, plen, accessible| {
            let prot = if accessible {
                libc::PROT_READ | libc::PROT_WRITE
            } else {
                libc::PROT_NONE
            };
            // SAFETY: `[page_ptr, page_ptr+plen)` is a page-aligned sub-range of the
            // mapping this `HostRam` owns (the core rounds to `HOST_PAGE` before calling).
            let rc = unsafe { libc::mprotect(page_ptr as *mut libc::c_void, plen, prot) };
            debug_assert_eq!(
                rc,
                0,
                "mprotect failed: {}",
                std::io::Error::last_os_error()
            );
        })),
    }
}

fn munmap_dtor() -> Box<dyn FnMut(*mut u8, usize) + Send> {
    Box::new(|p, l| {
        // SAFETY: `p`/`l` are exactly the region we mapped; unmapped once, on drop.
        unsafe {
            libc::munmap(p as *mut libc::c_void, l);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86jit_core::{
        InterpreterBackend, MemConsistency, MemoryModel, Prot, RegionKind, Vm, VmConfig,
    };

    fn rss_bytes() -> u64 {
        let s = std::fs::read_to_string("/proc/self/statm").unwrap();
        let pages: u64 = s.split_whitespace().nth(1).unwrap().parse().unwrap();
        pages * 4096
    }

    #[test]
    fn reserve_at_gives_host_equals_guest_identity() {
        // A fixed-address sparse span at guest_base 0x10000: a guest address equals its
        // own host address (`ptr as u64 - guest_base == 0`). Reserve 16 GiB above the
        // base (NORESERVE, so no physical commit), map a low region, write through the
        // VM, and read the same bytes via a raw host-pointer deref at the guest address.
        let guest_base = 0x10000u64;
        let span = 16u64 << 30; // exclusive top guest address
        let ram = reserve_at(guest_base, span);
        assert_eq!(ram.guest_base, guest_base);
        assert_eq!(ram.ptr as u64, guest_base, "fixed mmap lands at guest_base");
        let mut vm =
            Vm::with_backend_host_ram(VmConfig::reserved(span), Box::new(InterpreterBackend), ram);
        vm.map(0x400000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        vm.write_bytes(0x400000, &[0xB8, 0x2A, 0x00, 0x00, 0x00, 0xF4])
            .unwrap();
        // Embedder-side identity: the guest address is a live host address holding the
        // guest's bytes. SAFETY: 0x400000 was mapped + written above, so under identity
        // mapping it is a valid host address.
        assert_eq!(unsafe { *(0x400000u64 as *const u8) }, 0xB8);
        // And below-base guest addresses are rejected (no backing there).
        assert!(vm.map(0x8000, 0x1000, Prot::RW, RegionKind::Ram).is_err());
    }

    #[test]
    fn reserved_span_is_sparse_and_reaches_high_addresses() {
        // Reserve 512 GiB (impossible as an eager allocation on this box), map a low
        // region and a region at a 768 GiB arena hint, touch a few pages each.
        let span = 512u64 << 30;
        let hi = 400u64 << 30; // inside the 512 GiB span, far from the low region
        let before = rss_bytes();

        let ram = reserve(span);
        let mut vm = Vm::with_backend_host_ram(
            VmConfig {
                memory_model: MemoryModel::Reserved { span },
                consistency: MemConsistency::Fast,
            },
            Box::new(InterpreterBackend),
            ram,
        );
        vm.map(0x1000, 0x3000, Prot::RW, RegionKind::Ram).unwrap();
        vm.map(hi, 0x1000, Prot::RW, RegionKind::Ram).unwrap();

        // The one-add translation reaches a high sparse address, and round-trips.
        vm.mem.write(0x1000, 0x1122_3344, 8).unwrap();
        vm.mem.write(hi + 0x40, 0xdead_beef, 8).unwrap();
        assert_eq!(vm.mem.read(0x1000, 8).unwrap(), 0x1122_3344);
        assert_eq!(vm.mem.read(hi + 0x40, 8).unwrap(), 0xdead_beef);

        // Touching a handful of pages across a 512 GiB reservation must keep RSS tiny
        // (NORESERVE + demand paging; the bounded ~1 MiB code-page table dominates).
        let delta = rss_bytes().saturating_sub(before);
        assert!(
            delta < 20 * 1024 * 1024,
            "512 GiB reservation + a few touched pages grew RSS by {delta} bytes (want < 20 MiB)"
        );
        // The dtor `munmap`s the span when `vm` drops at end of scope.
    }

    #[test]
    fn reserve_guarded_maps_opened_regions_and_stays_sparse() {
        // A PROT_NONE span: mapped regions get mprotect'd RW (accessible), untouched
        // holes stay PROT_NONE (would fault — the point of doc-30, exercised in GP-2
        // with the signal handler; here we only touch mapped regions).
        let span = 512u64 << 30;
        let hi = 400u64 << 30;
        let before = rss_bytes();
        let ram = reserve_guarded(span);
        let mut vm = Vm::with_backend_host_ram(
            VmConfig {
                memory_model: MemoryModel::Reserved { span },
                consistency: MemConsistency::Fast,
            },
            Box::new(InterpreterBackend),
            ram,
        );
        vm.map(0x1000, 0x3000, Prot::RW, RegionKind::Ram).unwrap(); // opened → RW
        vm.map(hi, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        // Opened regions read/write like RW memory.
        vm.mem.write(0x1000, 0x1122_3344, 8).unwrap();
        vm.mem.write(hi + 0x40, 0xdead_beef, 8).unwrap();
        assert_eq!(vm.mem.read(0x1000, 8).unwrap(), 0x1122_3344);
        assert_eq!(vm.mem.read(hi + 0x40, 8).unwrap(), 0xdead_beef);
        // Still sparse: PROT_NONE + per-region mprotect doesn't commit the span.
        let delta = rss_bytes().saturating_sub(before);
        assert!(
            delta < 20 * 1024 * 1024,
            "guarded 512 GiB reservation grew RSS by {delta} bytes (want < 20 MiB)"
        );
    }
}
