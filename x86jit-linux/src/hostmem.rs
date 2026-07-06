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
        dtor: Box::new(|p, l| {
            // SAFETY: `p`/`l` are exactly the region we mapped; unmapped once, on drop.
            unsafe {
                libc::munmap(p as *mut libc::c_void, l);
            }
        }),
    }
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
}
