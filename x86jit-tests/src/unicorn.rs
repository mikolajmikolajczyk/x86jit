//! Unicorn differential oracle (testing.md §4) — the cross-platform source of
//! truth. Maps a `VectorInput` into a Unicorn X86-64 machine, runs it, and reads
//! the state back into a `RunOutcome`.
//!
//! Feature-gated (`unicorn`): links the system `libunicorn` via pkg-config, so
//! the core harness builds without the native library.
//!
//! Terminator handling: our snippets end in `hlt` (privileged — Unicorn would
//! fault if it executed it), so a code hook stops emulation *before* the `hlt`
//! (or `syscall`) and RIP is normalized to PAST the terminator to match the
//! engine's convention (§8: syscall/hlt resume past the instruction).

use std::cell::RefCell;
use std::rc::Rc;

use unicorn_engine::unicorn_const::{Arch, Mode, Prot};
use unicorn_engine::{RegisterX86, Unicorn};
use x86jit_core::CpuMode;

use crate::oracle::{Oracle, RunOutcome, VectorInput};
use crate::vector::{CpuSnapshot, ExitKind, MemChunk, RunSpec, SnapFlags};

const PAGE: u64 = 0x1000;

/// `gpr[]` slot order (x86 encoding) → Unicorn register id.
const GPR_REGS: [RegisterX86; 16] = [
    RegisterX86::RAX,
    RegisterX86::RCX,
    RegisterX86::RDX,
    RegisterX86::RBX,
    RegisterX86::RSP,
    RegisterX86::RBP,
    RegisterX86::RSI,
    RegisterX86::RDI,
    RegisterX86::R8,
    RegisterX86::R9,
    RegisterX86::R10,
    RegisterX86::R11,
    RegisterX86::R12,
    RegisterX86::R13,
    RegisterX86::R14,
    RegisterX86::R15,
];

/// 32-bit GPR register ids for `gpr[0..8]` (the only 8 GPRs a 32-bit guest has).
/// In `UC_MODE_32` the 64-bit register ids (RAX…) read/write as a no-op, so the
/// 32-bit lane must use the E-register ids for both load and read-back.
const GPR_REGS32: [RegisterX86; 8] = [
    RegisterX86::EAX,
    RegisterX86::ECX,
    RegisterX86::EDX,
    RegisterX86::EBX,
    RegisterX86::ESP,
    RegisterX86::EBP,
    RegisterX86::ESI,
    RegisterX86::EDI,
];

const XMM_REGS: [RegisterX86; 16] = [
    RegisterX86::XMM0,
    RegisterX86::XMM1,
    RegisterX86::XMM2,
    RegisterX86::XMM3,
    RegisterX86::XMM4,
    RegisterX86::XMM5,
    RegisterX86::XMM6,
    RegisterX86::XMM7,
    RegisterX86::XMM8,
    RegisterX86::XMM9,
    RegisterX86::XMM10,
    RegisterX86::XMM11,
    RegisterX86::XMM12,
    RegisterX86::XMM13,
    RegisterX86::XMM14,
    RegisterX86::XMM15,
];

#[derive(Clone, Copy)]
enum Term {
    Hlt,
    Syscall,
}

/// The Unicorn oracle in x86-64 long mode (`UC_MODE_64`) — the default lane the
/// whole 64-bit corpus validates against.
pub struct UnicornOracle;

impl Oracle for UnicornOracle {
    fn name(&self) -> &str {
        "unicorn"
    }

    fn run(&self, input: &VectorInput) -> RunOutcome {
        run_unicorn(input, CpuMode::Long64)
    }
}

/// The Unicorn oracle in 32-bit protected/compat mode (`UC_MODE_32`) — the mode-A
/// (task-197) lane. Same execution shape as [`UnicornOracle`]; the mode is a
/// parameter to [`run_unicorn`], not a fork of the machinery.
pub struct UnicornOracle32;

impl Oracle for UnicornOracle32 {
    fn name(&self) -> &str {
        "unicorn32"
    }

    fn run(&self, input: &VectorInput) -> RunOutcome {
        run_unicorn(input, CpuMode::Compat32)
    }
}

/// Map a `VectorInput` into a Unicorn machine at the given guest mode, run it, and
/// read the state back. `UC_MODE_32` decodes the same byte stream as a 32-bit
/// guest (0x40–0x4F = inc/dec, no REX, 67h = 16-bit addressing, address wrap at
/// 4 GiB), so the same corpus tables that are mode-neutral run on both lanes.
fn run_unicorn(input: &VectorInput, mode: CpuMode) -> RunOutcome {
    match mode {
        CpuMode::Long64 => run_unicorn_impl(input, Mode::MODE_64),
        CpuMode::Compat32 => run_unicorn_impl(input, Mode::MODE_32),
    }
}

fn run_unicorn_impl(input: &VectorInput, uc_mode: Mode) -> RunOutcome {
    let mut uc = Unicorn::new(Arch::X86, uc_mode).expect("open unicorn x86");

    for page in pages_for(&input.mem_init) {
        uc.mem_map(page, PAGE, Prot::ALL).expect("map guest page");
    }
    for chunk in &input.mem_init {
        uc.mem_write(chunk.addr, &chunk.bytes)
            .expect("write guest bytes");
    }

    let bits32 = uc_mode == Mode::MODE_32;
    load_regs(&mut uc, &input.cpu_init, input.entry, bits32);

    // Stop before a terminating hlt/syscall; record which and where.
    let term: Rc<RefCell<Option<(u64, u8, Term)>>> = Rc::new(RefCell::new(None));
    let term_hook = term.clone();
    uc.add_code_hook(input.entry, u64::MAX, move |uc, addr, size| {
        let mut buf = vec![0u8; size as usize];
        if uc.mem_read(addr, &mut buf).is_err() {
            return;
        }
        let hit = if size == 1 && buf[0] == 0xf4 {
            Some(Term::Hlt)
        } else if size == 2 && buf[0] == 0x0f && buf[1] == 0x05 {
            Some(Term::Syscall)
        } else {
            None
        };
        if let Some(kind) = hit {
            *term_hook.borrow_mut() = Some((addr, size as u8, kind));
            let _ = uc.emu_stop();
        }
    })
    .expect("install terminator hook");

    // M1 corpus is hlt-terminated (UntilExit). Cap instruction count as a
    // runaway guard; Blocks(N) alignment with Unicorn is a later concern.
    let count = match input.run {
        RunSpec::Blocks(n) => n as usize,
        RunSpec::UntilExit => 1_000_000,
    };
    let run_result = uc.emu_start(input.entry, u64::MAX, 0, count);

    let term = *term.borrow();
    let (rip_override, exit) = match term {
        Some((addr, len, Term::Hlt)) => (Some(addr + len as u64), ExitKind::Hlt),
        Some((addr, len, Term::Syscall)) => (Some(addr + len as u64), ExitKind::Syscall),
        None => (None, exit_from_result(&uc, &run_result)),
    };

    RunOutcome {
        cpu: store_regs(&uc, rip_override, bits32),
        mem: read_back(&uc, &input.mem_init),
        exit,
    }
}

/// Unique 4-KiB pages spanned by all chunks.
fn pages_for(chunks: &[MemChunk]) -> Vec<u64> {
    let mut pages = Vec::new();
    for c in chunks {
        let first = c.addr & !(PAGE - 1);
        let last = (c.addr + c.bytes.len() as u64 - 1) & !(PAGE - 1);
        let mut p = first;
        while p <= last {
            if !pages.contains(&p) {
                pages.push(p);
            }
            p += PAGE;
        }
    }
    pages
}

fn load_regs(uc: &mut Unicorn<()>, snap: &CpuSnapshot, entry: u64, bits32: bool) {
    if bits32 {
        // Only the 8 legacy GPRs exist; EIP/EFLAGS are the 32-bit PC/flags. The
        // 64-bit RIP/RFLAGS ids are no-ops here (they'd leave EIP at 0).
        for (reg, &v) in GPR_REGS32.iter().zip(&snap.gpr) {
            uc.reg_write(*reg, v & 0xffff_ffff).unwrap();
        }
        uc.reg_write(RegisterX86::EIP, entry & 0xffff_ffff).unwrap();
        uc.reg_write(RegisterX86::EFLAGS, snap.flags.to_rflags())
            .unwrap();
    } else {
        for (reg, &v) in GPR_REGS.iter().zip(&snap.gpr) {
            uc.reg_write(*reg, v).unwrap();
        }
        uc.reg_write(RegisterX86::RIP, entry).unwrap();
        uc.reg_write(RegisterX86::FS_BASE, snap.fs_base).unwrap();
        uc.reg_write(RegisterX86::GS_BASE, snap.gs_base).unwrap();
        uc.reg_write(RegisterX86::RFLAGS, snap.flags.to_rflags())
            .unwrap();
    }
    for (reg, v) in XMM_REGS.iter().zip(&snap.xmm) {
        uc.reg_write_long(*reg, &v.to_le_bytes()).unwrap();
    }
}

fn store_regs(uc: &Unicorn<()>, rip_override: Option<u64>, bits32: bool) -> CpuSnapshot {
    if bits32 {
        return store_regs32(uc, rip_override);
    }
    let mut gpr = [0u64; 16];
    for (slot, reg) in gpr.iter_mut().zip(&GPR_REGS) {
        *slot = uc.reg_read(*reg).unwrap();
    }
    let mut xmm = [0u128; 16];
    for (slot, reg) in xmm.iter_mut().zip(&XMM_REGS) {
        let bytes = uc.reg_read_long(*reg).unwrap();
        let mut b = [0u8; 16];
        b.copy_from_slice(&bytes[..16]);
        *slot = u128::from_le_bytes(b);
    }
    CpuSnapshot {
        gpr,
        rip: rip_override.unwrap_or_else(|| uc.reg_read(RegisterX86::RIP).unwrap()),
        flags: SnapFlags::from_rflags(uc.reg_read(RegisterX86::RFLAGS).unwrap()),
        fs_base: uc.reg_read(RegisterX86::FS_BASE).unwrap(),
        gs_base: uc.reg_read(RegisterX86::GS_BASE).unwrap(),
        xmm,
        // This Unicorn build can't run AVX (task-168.2), so it never sets YMM upper
        // halves; leave them zero. AVX tests use the interpreter, not this oracle.
        ymm_hi: [0; 16],
        // Likewise no AVX-512 state (task-193); ZMM upper halves and opmasks stay zero.
        zmm_hi: [[0; 2]; 16],
        kmask: [0; 8],
    }
}

/// Read back a 32-bit-mode machine: only the 8 legacy GPRs (as `gpr[0..8]`), EIP,
/// and EFLAGS exist — `gpr[8..16]` (r8–r15) stay zero, matching the interpreter's
/// Compat32 snapshot where those registers are equally out of reach.
fn store_regs32(uc: &Unicorn<()>, rip_override: Option<u64>) -> CpuSnapshot {
    let mut gpr = [0u64; 16];
    for (slot, reg) in gpr.iter_mut().zip(&GPR_REGS32) {
        *slot = uc.reg_read(*reg).unwrap() & 0xffff_ffff;
    }
    let mut xmm = [0u128; 16];
    for (slot, reg) in xmm.iter_mut().zip(&XMM_REGS) {
        let bytes = uc.reg_read_long(*reg).unwrap();
        let mut b = [0u8; 16];
        b.copy_from_slice(&bytes[..16]);
        *slot = u128::from_le_bytes(b);
    }
    CpuSnapshot {
        gpr,
        rip: rip_override
            .map(|r| r & 0xffff_ffff)
            .unwrap_or_else(|| uc.reg_read(RegisterX86::EIP).unwrap() & 0xffff_ffff),
        flags: SnapFlags::from_rflags(uc.reg_read(RegisterX86::EFLAGS).unwrap()),
        fs_base: 0,
        gs_base: 0,
        xmm,
        ymm_hi: [0; 16],
        zmm_hi: [[0; 2]; 16],
        kmask: [0; 8],
    }
}

fn read_back(uc: &Unicorn<()>, chunks: &[MemChunk]) -> Vec<MemChunk> {
    chunks
        .iter()
        .map(|c| MemChunk {
            addr: c.addr,
            bytes: uc.mem_read_as_vec(c.addr, c.bytes.len()).unwrap(),
            kind: c.kind,
        })
        .collect()
}

/// Best-effort exit when no terminator fired (the M1 corpus never reaches here).
fn exit_from_result(uc: &Unicorn<()>, result: &Result<(), unicorn_engine::uc_error>) -> ExitKind {
    use crate::vector::Access;
    let rip = uc.reg_read(RegisterX86::RIP).unwrap_or(0);
    match result {
        Ok(()) => ExitKind::Budget,
        Err(_) => ExitKind::UnmappedMemory {
            addr: rip,
            access: Access::Read,
        },
    }
}
