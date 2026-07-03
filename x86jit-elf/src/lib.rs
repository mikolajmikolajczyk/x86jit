//! Optional ELF loader helper (§2, §4.2).
//!
//! Lives OUTSIDE the core: the engine never parses file formats. This crate is
//! a convenience that maps ELF `PT_LOAD` segments into a `Vm`, sets up a System V
//! initial stack, and returns the entry point (§1 boundary rule). A user may
//! replace it entirely.
//!
//! Scope: static, little-endian, x86-64. Parsing is delegated to `goblin`;
//! dynamic linking, relocations, and TLS setup remain the embedder's job.

use goblin::elf::header::EM_X86_64;
use goblin::elf::program_header::{PF_W, PF_X, PT_LOAD};
use goblin::elf::Elf;

use x86jit_core::{Prot, RegionKind, Vm};

#[derive(Debug)]
pub enum LoadError {
    /// `goblin` could not parse the bytes as an ELF.
    NotElf(goblin::error::Error),
    /// Parsed, but not the supported class/encoding/machine (64-bit LE x86-64).
    Unsupported,
    /// A segment's file range runs past the end of the buffer.
    Truncated,
    /// `vm.map` / `vm.write_bytes` rejected a segment (out of bounds / overlap).
    Map,
}

/// Map a static x86-64 ELF's load segments into `vm`, returning the entry point
/// to place in `Reg::Rip` (§4.3, M2).
pub fn load_static_elf(vm: &mut Vm, bytes: &[u8]) -> Result<u64, LoadError> {
    let elf = Elf::parse(bytes).map_err(LoadError::NotElf)?;
    if !elf.is_64 || !elf.little_endian || elf.header.e_machine != EM_X86_64 {
        return Err(LoadError::Unsupported);
    }

    for ph in &elf.program_headers {
        if ph.p_type != PT_LOAD {
            continue;
        }
        // Reserve the whole segment (memsz ≥ filesz; the tail is bss and the flat
        // buffer is already zero-initialized).
        vm.map(ph.p_vaddr, ph.p_memsz as usize, prot(ph.p_flags), RegionKind::Ram)
            .map_err(|_| LoadError::Map)?;

        let start = ph.p_offset as usize;
        let end = start
            .checked_add(ph.p_filesz as usize)
            .ok_or(LoadError::Truncated)?;
        let data = bytes.get(start..end).ok_or(LoadError::Truncated)?;
        vm.write_bytes(ph.p_vaddr, data).map_err(|_| LoadError::Map)?;
    }

    Ok(elf.entry)
}

// System V AMD64 auxiliary-vector entry types (a minimal subset).
const AT_NULL: u64 = 0;
const AT_PAGESZ: u64 = 6;
const PAGE_SIZE: u64 = 4096;

/// Build the System V AMD64 initial process stack in guest memory and return the
/// `Rsp` to start `_start` with. Layout at entry (low → high):
///
/// ```text
/// rsp → argc, argv[0..], NULL, envp[0..], NULL, auxv pairs.., AT_NULL, [strings above]
/// ```
///
/// `rsp` is 16-byte aligned (ABI requirement at `_start`). Strings are written at
/// the top of the stack region and the pointer vector just below them. The stack
/// region must already be mapped up to `stack_top`.
pub fn setup_stack(
    vm: &mut Vm,
    stack_top: u64,
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Result<u64, LoadError> {
    // 1. Copy the argument/environment strings near the top, top-down.
    let mut p = stack_top;
    let argv_ptrs = push_strings(vm, &mut p, argv)?;
    let envp_ptrs = push_strings(vm, &mut p, envp)?;

    // 2. Size the pointer vector: argc + argv + NULL + envp + NULL + auxv + AT_NULL.
    let auxv: [(u64, u64); 2] = [(AT_PAGESZ, PAGE_SIZE), (AT_NULL, 0)];
    let words = 1 + argv_ptrs.len() + 1 + envp_ptrs.len() + 1 + auxv.len() * 2;
    let rsp = (p - words as u64 * 8) & !0xf;

    // 3. Write it upward from rsp.
    let mut at = rsp;
    write_word(vm, &mut at, argv.len() as u64)?; // argc
    for &ptr in &argv_ptrs {
        write_word(vm, &mut at, ptr)?;
    }
    write_word(vm, &mut at, 0)?; // argv terminator
    for &ptr in &envp_ptrs {
        write_word(vm, &mut at, ptr)?;
    }
    write_word(vm, &mut at, 0)?; // envp terminator
    for (kind, val) in auxv {
        write_word(vm, &mut at, kind)?;
        write_word(vm, &mut at, val)?;
    }

    Ok(rsp)
}

/// Write each NUL-terminated string below `*p`, returning their guest addresses.
fn push_strings(vm: &mut Vm, p: &mut u64, strings: &[&[u8]]) -> Result<Vec<u64>, LoadError> {
    let mut ptrs = Vec::with_capacity(strings.len());
    for s in strings {
        *p -= s.len() as u64 + 1;
        vm.write_bytes(*p, s).map_err(|_| LoadError::Map)?;
        vm.write_bytes(*p + s.len() as u64, &[0]).map_err(|_| LoadError::Map)?;
        ptrs.push(*p);
    }
    Ok(ptrs)
}

fn write_word(vm: &mut Vm, at: &mut u64, val: u64) -> Result<(), LoadError> {
    vm.write_bytes(*at, &val.to_le_bytes()).map_err(|_| LoadError::Map)?;
    *at += 8;
    Ok(())
}

/// ELF `p_flags` → `Prot`. `Prot` has no write/exec-only forms, so anything with
/// X folds to `RX`/`RWX` and anything with W (no X) to `RW`.
fn prot(flags: u32) -> Prot {
    match (flags & PF_X != 0, flags & PF_W != 0) {
        (true, true) => Prot::RWX,
        (true, false) => Prot::RX,
        (false, true) => Prot::RW,
        (false, false) => Prot::R,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86jit_core::{MemConsistency, MemoryModel, VmConfig};

    fn vm_with_stack(base: u64, size: u64) -> Vm {
        let mut vm = Vm::new(VmConfig {
            memory_model: MemoryModel::Flat { size: base + size },
            consistency: MemConsistency::Fast,
        });
        vm.map(base, size as usize, Prot::RW, RegionKind::Ram).unwrap();
        vm
    }

    fn read_u64(vm: &Vm, at: u64) -> u64 {
        let mut b = [0u8; 8];
        vm.read_bytes(at, &mut b).unwrap();
        u64::from_le_bytes(b)
    }

    #[test]
    fn stack_layout_is_sysv() {
        let top = 0x10000u64;
        let mut vm = vm_with_stack(0x8000, 0x8000);
        let rsp = setup_stack(&mut vm, top, &[b"prog", b"arg1"], &[b"PATH=/bin"]).unwrap();

        assert_eq!(rsp % 16, 0, "rsp must be 16-byte aligned at _start");

        // argc
        assert_eq!(read_u64(&vm, rsp), 2);
        // argv[0], argv[1], NULL
        let a0 = read_u64(&vm, rsp + 8);
        let a1 = read_u64(&vm, rsp + 16);
        assert_eq!(read_u64(&vm, rsp + 24), 0, "argv terminator");
        // envp[0], NULL
        let e0 = read_u64(&vm, rsp + 32);
        assert_eq!(read_u64(&vm, rsp + 40), 0, "envp terminator");
        // auxv: AT_PAGESZ, 4096, then AT_NULL
        assert_eq!(read_u64(&vm, rsp + 48), AT_PAGESZ);
        assert_eq!(read_u64(&vm, rsp + 56), PAGE_SIZE);
        assert_eq!(read_u64(&vm, rsp + 64), AT_NULL);

        // Pointers resolve to the right NUL-terminated strings.
        let read_cstr = |at: u64| {
            let mut out = Vec::new();
            let mut a = at;
            loop {
                let mut b = [0u8; 1];
                vm.read_bytes(a, &mut b).unwrap();
                if b[0] == 0 {
                    break;
                }
                out.push(b[0]);
                a += 1;
            }
            out
        };
        assert_eq!(read_cstr(a0), b"prog");
        assert_eq!(read_cstr(a1), b"arg1");
        assert_eq!(read_cstr(e0), b"PATH=/bin");
    }
}
