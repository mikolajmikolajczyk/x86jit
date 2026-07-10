//! Optional ELF loader helper (§2, §4.2).
//!
//! Lives OUTSIDE the core: the engine never parses file formats. This crate is
//! a convenience that maps ELF `PT_LOAD` segments into a `Vm`, sets up a System V
//! initial stack, and returns the entry point (§1 boundary rule). A user may
//! replace it entirely.
//!
//! Scope: static, little-endian, x86-64. Parsing is delegated to `goblin`;
//! dynamic linking, relocations, and TLS setup remain the embedder's job.

use goblin::elf::header::{EM_386, EM_X86_64};
use goblin::elf::program_header::PT_LOAD;
use goblin::elf::Elf;

use x86jit_core::{Prot, RegionKind, Vm};

#[derive(Debug)]
pub enum LoadError {
    /// `goblin` could not parse the bytes as an ELF.
    NotElf(goblin::error::Error),
    /// Parsed, but not a supported class/encoding/machine — with a clear reason
    /// (§17.7 loud rejection). E.g. a big-endian file, or an ELFCLASS32 that is not
    /// `EM_386`, or an ELFCLASS64 that is not `EM_X86_64`.
    Unsupported(&'static str),
    /// A segment's file range runs past the end of the buffer.
    Truncated,
    /// `vm.map` / `vm.write_bytes` rejected a segment (out of bounds / overlap).
    Map,
}

/// Map a static x86-64 ELF's load segments into `vm`, returning the entry point
/// to place in `Reg::Rip` (§4.3, M2).
pub fn load_static_elf(vm: &mut Vm, bytes: &[u8]) -> Result<u64, LoadError> {
    let elf = Elf::parse(bytes).map_err(LoadError::NotElf)?;
    reject_unless_x86_64(&elf)?;
    map_segments(vm, &elf, bytes, 0)?;
    Ok(elf.entry)
}

/// Map a static **i386** (`ELFCLASS32` + `EM_386`) ELF's load segments into `vm`,
/// returning the 32-bit entry point (§17.7). The image maps at its own vaddrs
/// (below 4 GiB by construction — an i386 executable's addresses fit in 32 bits).
/// The caller must have set [`Vm::set_cpu_mode`]`(CpuMode::Compat32)` and must build
/// the initial stack with [`setup_stack_i386`] (4-byte slots). Non-i386 32-bit or
/// big-endian files are rejected loudly.
pub fn load_static_elf_i386(vm: &mut Vm, bytes: &[u8]) -> Result<u64, LoadError> {
    let elf = Elf::parse(bytes).map_err(LoadError::NotElf)?;
    reject_unless_i386(&elf)?;
    map_segments(vm, &elf, bytes, 0)?;
    Ok(elf.entry)
}

/// §17.7 loud rejection for the 64-bit path: only little-endian `ELFCLASS64` +
/// `EM_X86_64` is supported; everything else gets a clear reason.
fn reject_unless_x86_64(elf: &Elf) -> Result<(), LoadError> {
    if !elf.little_endian {
        return Err(LoadError::Unsupported(
            "big-endian ELF: only little-endian x86 is supported",
        ));
    }
    if !elf.is_64 {
        return Err(LoadError::Unsupported(
            "32-bit ELF passed to the 64-bit loader: use load_static_elf_i386 for i386",
        ));
    }
    if elf.header.e_machine != EM_X86_64 {
        return Err(LoadError::Unsupported(
            "unsupported machine: only EM_X86_64 (x86-64 long mode) supported here",
        ));
    }
    Ok(())
}

/// §17.7 loud rejection for the i386 path: only little-endian `ELFCLASS32` +
/// `EM_386` is supported. A 64-bit file, a big-endian file, or a non-i386 32-bit
/// machine (`EM_ARM`, `EM_MIPS`, …) is refused with a clear reason.
fn reject_unless_i386(elf: &Elf) -> Result<(), LoadError> {
    if !elf.little_endian {
        return Err(LoadError::Unsupported(
            "big-endian ELF: only little-endian x86 is supported",
        ));
    }
    if elf.is_64 {
        return Err(LoadError::Unsupported(
            "64-bit ELF passed to the i386 loader: use load_static_elf for x86-64",
        ));
    }
    if elf.header.e_machine != EM_386 {
        return Err(LoadError::Unsupported(
            "unsupported 32-bit machine: only EM_386 (Linux i386) supported in compat mode",
        ));
    }
    Ok(())
}

const PAGE: u64 = 4096;

/// Map one ELF's `PT_LOAD` segments at `base + p_vaddr` (base = 0 for `ET_EXEC`;
/// the load bias for a `ET_DYN` PIE / interpreter).
///
/// The whole image span is reserved as **one page-aligned region**, then each
/// segment's file bytes are written in. This matches the kernel's page-granular
/// mapping (a dynamic loader writes relocations up to a segment's page boundary,
/// past its exact `memsz`) and sidesteps per-segment page-overlap. Protections
/// aren't enforced in the flat model (§4.2), so a single RW mapping is fine.
fn map_segments(vm: &mut Vm, elf: &Elf, bytes: &[u8], base: u64) -> Result<(), LoadError> {
    let loads: Vec<_> = elf
        .program_headers
        .iter()
        .filter(|p| p.p_type == PT_LOAD)
        .collect();
    let lo = loads
        .iter()
        .map(|p| base + p.p_vaddr)
        .min()
        .ok_or(LoadError::Unsupported("ELF has no PT_LOAD segments"))?;
    let hi = loads
        .iter()
        .map(|p| base + p.p_vaddr + p.p_memsz)
        .max()
        .ok_or(LoadError::Unsupported("ELF has no PT_LOAD segments"))?;
    let start = lo & !(PAGE - 1);
    let end = hi.div_ceil(PAGE) * PAGE;
    vm.map(start, (end - start) as usize, Prot::RW, RegionKind::Ram)
        .map_err(|_| LoadError::Map)?;

    for ph in loads {
        let fstart = ph.p_offset as usize;
        let fend = fstart
            .checked_add(ph.p_filesz as usize)
            .ok_or(LoadError::Truncated)?;
        let data = bytes.get(fstart..fend).ok_or(LoadError::Truncated)?;
        vm.write_bytes(base + ph.p_vaddr, data)
            .map_err(|_| LoadError::Map)?;
    }
    Ok(())
}

/// What a dynamic load produces: the entry point to jump to (the interpreter's)
/// plus the auxv values the interpreter needs to find and relocate the program.
#[derive(Copy, Clone, Debug)]
pub struct DynImage {
    /// Interpreter entry (`interp_base + interp.e_entry`) — where `_start` begins.
    pub entry: u64,
    /// `AT_PHDR`: program headers of the *executable* in guest memory.
    pub phdr: u64,
    pub phent: u64,
    pub phnum: u64,
    /// `AT_BASE`: the interpreter's load bias.
    pub base: u64,
    /// `AT_ENTRY`: the executable's own entry (where the interpreter jumps after
    /// relocation).
    pub exec_entry: u64,
}

/// Load a dynamically-linked x86-64 ELF (`ET_DYN` PIE) and its interpreter
/// (`ld-musl`/`ld-linux`) into `vm` at the given load biases, returning the info
/// needed to build the initial stack. The interpreter — real guest code — then
/// performs the relocations itself (§1: the engine never links). `interp_bytes`
/// is the interpreter file the embedder read from the host (the path is in the
/// executable's `PT_INTERP`).
pub fn load_dynamic_elf(
    vm: &mut Vm,
    exe_bytes: &[u8],
    exe_base: u64,
    interp_bytes: &[u8],
    interp_base: u64,
) -> Result<DynImage, LoadError> {
    let exe = Elf::parse(exe_bytes).map_err(LoadError::NotElf)?;
    let interp = Elf::parse(interp_bytes).map_err(LoadError::NotElf)?;
    reject_unless_x86_64(&exe)?;
    map_segments(vm, &exe, exe_bytes, exe_base)?;
    map_segments(vm, &interp, interp_bytes, interp_base)?;

    Ok(DynImage {
        entry: interp_base + interp.entry,
        // The program headers sit at file offset e_phoff, which the first PT_LOAD
        // maps at that same offset from the load bias.
        phdr: exe_base + exe.header.e_phoff,
        phent: exe.header.e_phentsize as u64,
        phnum: exe.header.e_phnum as u64,
        base: interp_base,
        exec_entry: exe_base + exe.entry,
    })
}

/// Load a **static-PIE** x86-64 ELF (`ET_DYN` with no `PT_INTERP` — e.g. a
/// static-musl binary) at `base`, returning the info to build its stack. There is
/// no interpreter: the binary's own `_start` applies its `R_X86_64_RELATIVE`
/// relocations using the auxv (§1: the engine never links). Enter at `entry`;
/// `AT_BASE` is 0 (no interpreter present).
pub fn load_static_pie_elf(vm: &mut Vm, bytes: &[u8], base: u64) -> Result<DynImage, LoadError> {
    let elf = Elf::parse(bytes).map_err(LoadError::NotElf)?;
    reject_unless_x86_64(&elf)?;
    map_segments(vm, &elf, bytes, base)?;
    Ok(DynImage {
        entry: base + elf.entry,
        phdr: base + elf.header.e_phoff,
        phent: elf.header.e_phentsize as u64,
        phnum: elf.header.e_phnum as u64,
        base: 0, // AT_BASE: no interpreter
        exec_entry: base + elf.entry,
    })
}

/// Whether `bytes` is a static-PIE executable (`ET_DYN` without a `PT_INTERP`),
/// which loads via [`load_static_pie_elf`] rather than [`load_static_elf`] (which
/// handles `ET_EXEC`) or [`load_dynamic_elf`] (which needs an interpreter).
pub fn is_static_pie(bytes: &[u8]) -> bool {
    use goblin::elf::header::ET_DYN;
    Elf::parse(bytes)
        .map(|e| e.header.e_type == ET_DYN && e.interpreter.is_none())
        .unwrap_or(false)
}

/// Path in the executable's `PT_INTERP` (the dynamic loader to map), if any.
pub fn interp_path(bytes: &[u8]) -> Option<String> {
    let elf = Elf::parse(bytes).ok()?;
    elf.interpreter.map(|s| s.to_string())
}

/// The unbiased `[lo, hi)` virtual-address span covering every `PT_LOAD` segment
/// (`hi` is the max `p_vaddr + p_memsz`). An embedder uses this to place a second
/// image (the interpreter) clear of the executable's own span — a big PIE loaded
/// at `EXE_BASE` can otherwise collide with a fixed interpreter base. Returns
/// `None` if the bytes don't parse or have no `PT_LOAD`.
pub fn load_span(bytes: &[u8]) -> Option<(u64, u64)> {
    let elf = Elf::parse(bytes).ok()?;
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    for p in elf.program_headers.iter().filter(|p| p.p_type == PT_LOAD) {
        lo = lo.min(p.p_vaddr);
        hi = hi.max(p.p_vaddr + p.p_memsz);
    }
    (lo <= hi).then_some((lo, hi))
}

/// True if the ELF carries a Go build-id note (owner `"Go"`) in a `PT_NOTE` segment.
/// The Go toolchain emits it, and — unlike the `.note.go.buildid` *section* — the
/// `PT_NOTE` segment survives `strip` / `-s -w`, so it is a reliable "this is a Go
/// runtime" signal. The runner keys off it to pick the big Reserved NORESERVE span +
/// threaded driver a Go program needs, leaving every other guest on the default Flat
/// space. Reserved must be **opt-in**, not the default: a Flat guest that `fork`s under
/// a host-backed Reserved span would panic the core (`Memory::fork` on host RAM), and a
/// Reserved span widens the JIT/interp unmapped-in-span divergence (decision-3) across
/// its whole address range. (go-caddy P1b.)
pub fn has_go_build_note(bytes: &[u8]) -> bool {
    let Ok(elf) = Elf::parse(bytes) else {
        return false;
    };
    let Some(notes) = elf.iter_note_headers(bytes) else {
        return false;
    };
    // Only the Go toolchain uses the owner name "Go"; the build-id note survives strip.
    // goblin keeps the note name's trailing NUL padding ("Go\0"), so trim before compare.
    notes
        .flatten()
        .any(|n| n.name.trim_end_matches('\0') == "Go")
}

// System V AMD64 auxiliary-vector entry types.
const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
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
    build_stack(vm, stack_top, argv, envp, &[])
}

/// Like [`setup_stack`] but with the auxv a dynamic executable's interpreter needs
/// (`AT_PHDR`/`AT_BASE`/`AT_ENTRY`, …) from [`load_dynamic_elf`]. The interpreter
/// reads these to locate and relocate the program.
pub fn setup_stack_dyn(
    vm: &mut Vm,
    stack_top: u64,
    argv: &[&[u8]],
    envp: &[&[u8]],
    img: &DynImage,
) -> Result<u64, LoadError> {
    let aux = [
        (AT_PHDR, img.phdr),
        (AT_PHENT, img.phent),
        (AT_PHNUM, img.phnum),
        (AT_BASE, img.base),
        (AT_ENTRY, img.exec_entry),
    ];
    build_stack(vm, stack_top, argv, envp, &aux)
}

/// Build the initial stack: strings, a 16-byte `AT_RANDOM` block, the pointer
/// vector (argc/argv/envp/auxv), 16-byte aligned. `extra_aux` is prepended to the
/// always-present `AT_PAGESZ`/`AT_RANDOM`/id/`AT_HWCAP` entries.
fn build_stack(
    vm: &mut Vm,
    stack_top: u64,
    argv: &[&[u8]],
    envp: &[&[u8]],
    extra_aux: &[(u64, u64)],
) -> Result<u64, LoadError> {
    // 1. Strings near the top, top-down, then 16 bytes for AT_RANDOM.
    let mut p = stack_top;
    let argv_ptrs = push_strings(vm, &mut p, argv)?;
    let envp_ptrs = push_strings(vm, &mut p, envp)?;
    p -= 16;
    let random_at = p;
    vm.write_bytes(random_at, &[0x5au8; 16])
        .map_err(|_| LoadError::Map)?; // fixed → deterministic

    // 2. Full auxv (terminated by AT_NULL).
    let mut auxv: Vec<(u64, u64)> = extra_aux.to_vec();
    auxv.extend_from_slice(&[
        (AT_PAGESZ, PAGE_SIZE),
        (AT_RANDOM, random_at),
        (AT_HWCAP, 0),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_NULL, 0),
    ]);

    // 3. Size + place the pointer vector, keeping the final rsp 16-aligned.
    let words = 1 + argv_ptrs.len() + 1 + envp_ptrs.len() + 1 + auxv.len() * 2;
    let mut rsp = p - words as u64 * 8;
    // rsp must be 16-aligned AND (words odd/even) land argc such that after the
    // whole vector the stack stays aligned; align down is sufficient for _start.
    rsp &= !0xf;

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

/// Build the **i386** System V initial process stack (4-byte slots) and return the
/// `Esp` to start `_start` with. Same shape as [`setup_stack`] — argc / argv[] /
/// NULL / envp[] / NULL / auxv pairs / AT_NULL — but every slot and every auxv value
/// is a 4-byte little-endian word, and the auxv entries are `Elf32` `(a_type, a_val)`
/// pairs. ESP is 16-byte aligned (glibc/musl i386 `_start` assumes it).
///
/// The caller placed the image with [`load_static_elf_i386`] and set the VM to
/// `CpuMode::Compat32`. The stack region must already be mapped up to `stack_top`,
/// which — like the whole i386 image — must live below 4 GiB.
pub fn setup_stack_i386(
    vm: &mut Vm,
    stack_top: u64,
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Result<u64, LoadError> {
    // 1. Strings near the top, then 16 bytes for AT_RANDOM.
    let mut p = stack_top;
    let argv_ptrs = push_strings(vm, &mut p, argv)?;
    let envp_ptrs = push_strings(vm, &mut p, envp)?;
    p -= 16;
    let random_at = p;
    vm.write_bytes(random_at, &[0x5au8; 16])
        .map_err(|_| LoadError::Map)?; // fixed → deterministic

    // 2. Full auxv (terminated by AT_NULL) — same entries as the 64-bit build, each
    //    value narrowed to a 32-bit slot below.
    let auxv: [(u64, u64); 10] = [
        (AT_PAGESZ, PAGE_SIZE),
        (AT_RANDOM, random_at),
        (AT_HWCAP, 0),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_NULL, 0),
    ];

    // 3. Size + place the pointer vector, keeping the final esp 16-aligned.
    let words = 1 + argv_ptrs.len() + 1 + envp_ptrs.len() + 1 + auxv.len() * 2;
    let mut esp = p - words as u64 * 4;
    esp &= !0xf;

    let mut at = esp;
    write_word32(vm, &mut at, argv.len() as u64)?; // argc
    for &ptr in &argv_ptrs {
        write_word32(vm, &mut at, ptr)?;
    }
    write_word32(vm, &mut at, 0)?; // argv terminator
    for &ptr in &envp_ptrs {
        write_word32(vm, &mut at, ptr)?;
    }
    write_word32(vm, &mut at, 0)?; // envp terminator
    for (kind, val) in auxv {
        write_word32(vm, &mut at, kind)?;
        write_word32(vm, &mut at, val)?;
    }

    Ok(esp)
}

/// Write each NUL-terminated string below `*p`, returning their guest addresses.
fn push_strings(vm: &mut Vm, p: &mut u64, strings: &[&[u8]]) -> Result<Vec<u64>, LoadError> {
    let mut ptrs = Vec::with_capacity(strings.len());
    for s in strings {
        *p -= s.len() as u64 + 1;
        vm.write_bytes(*p, s).map_err(|_| LoadError::Map)?;
        vm.write_bytes(*p + s.len() as u64, &[0])
            .map_err(|_| LoadError::Map)?;
        ptrs.push(*p);
    }
    Ok(ptrs)
}

fn write_word(vm: &mut Vm, at: &mut u64, val: u64) -> Result<(), LoadError> {
    vm.write_bytes(*at, &val.to_le_bytes())
        .map_err(|_| LoadError::Map)?;
    *at += 8;
    Ok(())
}

/// Write a 4-byte little-endian word for the i386 initial stack (§17.7). `val` is a
/// 32-bit quantity (pointer or auxv value) held in the low 32 bits.
fn write_word32(vm: &mut Vm, at: &mut u64, val: u64) -> Result<(), LoadError> {
    vm.write_bytes(*at, &(val as u32).to_le_bytes())
        .map_err(|_| LoadError::Map)?;
    *at += 4;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86jit_core::VmConfig;

    fn vm_with_stack(base: u64, size: u64) -> Vm {
        let mut vm = Vm::new(VmConfig::flat(base + size));
        vm.map(base, size as usize, Prot::RW, RegionKind::Ram)
            .unwrap();
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
        // auxv starts after the envp terminator: first pair is AT_PAGESZ.
        assert_eq!(read_u64(&vm, rsp + 48), AT_PAGESZ);
        assert_eq!(read_u64(&vm, rsp + 56), PAGE_SIZE);

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

    fn read_u32(vm: &Vm, at: u64) -> u32 {
        let mut b = [0u8; 4];
        vm.read_bytes(at, &mut b).unwrap();
        u32::from_le_bytes(b)
    }

    #[test]
    fn stack_layout_i386_is_4byte_sysv() {
        let top = 0x10000u64;
        let mut vm = vm_with_stack(0x8000, 0x8000);
        let esp = setup_stack_i386(&mut vm, top, &[b"prog", b"arg1"], &[b"PATH=/bin"]).unwrap();

        assert_eq!(esp % 16, 0, "esp must be 16-byte aligned at _start");

        // argc (4-byte slot), then argv[0], argv[1], NULL — all 4-byte.
        assert_eq!(read_u32(&vm, esp), 2, "argc");
        let a0 = read_u32(&vm, esp + 4) as u64;
        let a1 = read_u32(&vm, esp + 8) as u64;
        assert_eq!(read_u32(&vm, esp + 12), 0, "argv terminator");
        let e0 = read_u32(&vm, esp + 16) as u64;
        assert_eq!(read_u32(&vm, esp + 20), 0, "envp terminator");
        // First auxv pair (Elf32) is AT_PAGESZ = 6, value 4096, packed 4+4.
        assert_eq!(read_u32(&vm, esp + 24), AT_PAGESZ as u32, "auxv[0].a_type");
        assert_eq!(read_u32(&vm, esp + 28), PAGE_SIZE as u32, "auxv[0].a_val");

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

    #[test]
    fn i386_loader_rejects_x86_64_loudly() {
        // A hand-built ELF64 x86-64 header handed to the i386 loader is refused with a
        // clear reason (§17.7); the message names the 64-bit mismatch.
        let mut hdr = vec![0u8; 64];
        hdr[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        hdr[4] = 2; // ELFCLASS64
        hdr[5] = 1; // little-endian
        hdr[6] = 1;
        hdr[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        hdr[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        let mut vm = Vm::new(VmConfig::flat(0x1000));
        match load_static_elf_i386(&mut vm, &hdr) {
            Err(LoadError::Unsupported(msg)) => assert!(msg.contains("64-bit"), "reason: {msg}"),
            other => panic!("expected loud rejection, got {other:?}"),
        }
    }
}
