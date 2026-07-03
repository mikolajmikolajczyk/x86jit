//! Optional ELF loader helper (§2, §4.2).
//!
//! Lives OUTSIDE the core: the engine never parses file formats. This crate is
//! a convenience that maps ELF `PT_LOAD` segments into a `Vm` and returns the
//! entry point (§1 boundary rule). A user may replace it entirely.

use x86jit_core::Vm;

#[derive(Debug)]
pub enum LoadError {
    NotElf,
    Unsupported,
    Map,
}

/// Map a static x86-64 ELF's load segments into `vm`, returning the entry point
/// to place in `Reg::Rip` (§4.3, M2).
pub fn load_static_elf(_vm: &mut Vm, _bytes: &[u8]) -> Result<u64, LoadError> {
    todo!("M2: parse PT_LOAD segments, vm.map + vm.write_bytes each, return e_entry")
}
