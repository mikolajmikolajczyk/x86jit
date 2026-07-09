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

pub struct UnicornOracle;

impl Oracle for UnicornOracle {
    fn name(&self) -> &str {
        "unicorn"
    }

    fn run(&self, input: &VectorInput) -> RunOutcome {
        let mut uc = Unicorn::new(Arch::X86, Mode::MODE_64).expect("open unicorn x86-64");

        for page in pages_for(&input.mem_init) {
            uc.mem_map(page, PAGE, Prot::ALL).expect("map guest page");
        }
        for chunk in &input.mem_init {
            uc.mem_write(chunk.addr, &chunk.bytes)
                .expect("write guest bytes");
        }

        load_regs(&mut uc, &input.cpu_init, input.entry);

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
            cpu: store_regs(&uc, rip_override),
            mem: read_back(&uc, &input.mem_init),
            exit,
        }
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

fn load_regs(uc: &mut Unicorn<()>, snap: &CpuSnapshot, entry: u64) {
    for (reg, &v) in GPR_REGS.iter().zip(&snap.gpr) {
        uc.reg_write(*reg, v).unwrap();
    }
    uc.reg_write(RegisterX86::RIP, entry).unwrap();
    uc.reg_write(RegisterX86::FS_BASE, snap.fs_base).unwrap();
    uc.reg_write(RegisterX86::GS_BASE, snap.gs_base).unwrap();
    uc.reg_write(RegisterX86::RFLAGS, snap.flags.to_rflags())
        .unwrap();
    for (reg, v) in XMM_REGS.iter().zip(&snap.xmm) {
        uc.reg_write_long(*reg, &v.to_le_bytes()).unwrap();
    }
}

fn store_regs(uc: &Unicorn<()>, rip_override: Option<u64>) -> CpuSnapshot {
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
