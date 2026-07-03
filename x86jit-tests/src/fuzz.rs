//! Differential fuzzing (testing.md §7): generate random *valid* programs from
//! the supported instruction set, run them through two engines, and any state
//! divergence is a bug. Programs are structured (a `Vec<FuzzInsn>`) so a
//! divergence can be shrunk (§7.2) to a minimal reproducer, and the whole thing
//! is seed-deterministic (§7.3).
//!
//! Only pure computation — no syscalls/MMIO/branches to unmapped code — so runs
//! are reproducible. Memory operands are confined to a mapped scratch region.

use iced_x86::code_asm::*;

use crate::oracle::VectorInput;
use crate::vector::{CpuSnapshot, MemChunk, MemKind, RunSpec};

const CODE: u64 = 0x1000;
pub const SCRATCH: u64 = 0x8000;
const SCRATCH_LEN: usize = 0x1000;

/// Register pool (avoids RSP/RBP so a stray write can't wreck addressing). Index
/// `i` maps to `gpr[GPR_IDX[i]]` in the snapshot.
const GPR_IDX: [usize; 8] = [0, 3, 1, 2, 6, 7, 8, 9];
const POOL: usize = 8;

/// Deterministic PRNG (SplitMix64) — reproducible from a seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn reg(&mut self) -> u8 {
        self.below(POOL) as u8
    }
    fn size(&mut self) -> u8 {
        [4, 8, 4, 8, 1, 2][self.below(6)]
    }
    fn imm32(&mut self) -> i32 {
        const B: [i32; 8] = [0, 1, -1, i32::MAX, i32::MIN, 2, -2, 0x40];
        B[self.below(B.len())]
    }
    fn imm64(&mut self) -> u64 {
        const B: [u64; 8] = [0, 1, u64::MAX, i64::MAX as u64, 1 << 63, 0x1234_5678, 0xff, 0x8000_0000];
        B[self.below(B.len())]
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FuzzInsn {
    BinReg { op: u8, dst: u8, src: u8, size: u8 },
    BinImm { op: u8, dst: u8, imm: i32, size: u8 },
    UnReg { op: u8, dst: u8, size: u8 },
    MovImm { dst: u8, imm: u64, size: u8 },
    MovReg { dst: u8, src: u8, size: u8 },
    Movzx { dst: u8, src: u8 },
    Movsx { dst: u8, src: u8 },
    Setcc { cc: u8, dst: u8 },
    Cmov { cc: u8, dst: u8, src: u8 },
    Load { dst: u8, off: u16, size: u8 },
    Store { src: u8, off: u16, size: u8 },
}

#[derive(Clone)]
pub struct Prog {
    pub insns: Vec<FuzzInsn>,
    pub init: CpuSnapshot,
    pub seed: u64,
}

/// Generate a random program of `len` instructions from `seed`.
pub fn gen(seed: u64, len: usize) -> Prog {
    let mut rng = Rng::new(seed);
    let mut insns = Vec::with_capacity(len);
    for _ in 0..len {
        insns.push(gen_insn(&mut rng));
    }
    let mut init = CpuSnapshot { rip: CODE, ..Default::default() };
    for &gi in &GPR_IDX {
        init.gpr[gi] = rng.imm64();
    }
    Prog { insns, init, seed }
}

fn gen_insn(rng: &mut Rng) -> FuzzInsn {
    match rng.below(11) {
        0 => FuzzInsn::BinReg { op: rng.below(9) as u8, dst: rng.reg(), src: rng.reg(), size: rng.size() },
        1 => FuzzInsn::BinImm { op: rng.below(9) as u8, dst: rng.reg(), imm: rng.imm32(), size: rng.size() },
        2 => FuzzInsn::UnReg { op: rng.below(4) as u8, dst: rng.reg(), size: rng.size() },
        3 => FuzzInsn::MovImm { dst: rng.reg(), imm: rng.imm64(), size: rng.size() },
        4 => FuzzInsn::MovReg { dst: rng.reg(), src: rng.reg(), size: rng.size() },
        5 => FuzzInsn::Movzx { dst: rng.reg(), src: rng.reg() },
        6 => FuzzInsn::Movsx { dst: rng.reg(), src: rng.reg() },
        7 => FuzzInsn::Setcc { cc: rng.below(16) as u8, dst: rng.reg() },
        8 => FuzzInsn::Cmov { cc: rng.below(16) as u8, dst: rng.reg(), src: rng.reg() },
        9 => FuzzInsn::Load { dst: rng.reg(), off: (rng.below(SCRATCH_LEN - 8)) as u16, size: rng.size() },
        _ => FuzzInsn::Store { src: rng.reg(), off: (rng.below(SCRATCH_LEN - 8)) as u16, size: rng.size() },
    }
}

impl Prog {
    /// Assemble to a runnable input (append `hlt`; map code + a scratch region).
    pub fn input(&self) -> VectorInput {
        let mut a = CodeAssembler::new(64).unwrap();
        for insn in &self.insns {
            emit(&mut a, insn);
        }
        a.hlt().unwrap();
        let code = a.assemble(CODE).unwrap();

        VectorInput {
            cpu_init: self.init.clone(),
            mem_init: vec![
                MemChunk { addr: CODE, bytes: code, kind: MemKind::Ram },
                MemChunk { addr: SCRATCH, bytes: vec![0u8; SCRATCH_LEN], kind: MemKind::Ram },
            ],
            entry: CODE,
            run: RunSpec::UntilExit,
        }
    }
}

/// Shrink a diverging program to a minimal one still triggering `diverges`
/// (delta-debugging, §7.2): drop instructions, then zero init registers.
pub fn shrink(prog: &Prog, diverges: &mut impl FnMut(&Prog) -> bool) -> Prog {
    let mut best = prog.clone();
    loop {
        let mut improved = false;
        for i in 0..best.insns.len() {
            let mut cand = best.clone();
            cand.insns.remove(i);
            if diverges(&cand) {
                best = cand;
                improved = true;
                break;
            }
        }
        if !improved {
            break;
        }
    }
    for &gi in &GPR_IDX {
        if best.init.gpr[gi] == 0 {
            continue;
        }
        let mut cand = best.clone();
        cand.init.gpr[gi] = 0;
        if diverges(&cand) {
            best = cand;
        }
    }
    best
}

fn emit(a: &mut CodeAssembler, insn: &FuzzInsn) {
    match *insn {
        FuzzInsn::BinReg { op, dst, src, size } => bin_reg(a, op, dst, src, size),
        FuzzInsn::BinImm { op, dst, imm, size } => bin_imm(a, op, dst, imm, size),
        FuzzInsn::UnReg { op, dst, size } => un_reg(a, op, dst, size),
        FuzzInsn::MovImm { dst, imm, size } => match size {
            8 => a.mov(reg64(dst), imm).unwrap(),
            2 => a.mov(reg16(dst), (imm as u32 & 0xffff) as i32).unwrap(),
            1 => a.mov(reg8(dst), (imm as u32 & 0xff) as i32).unwrap(),
            _ => a.mov(reg32(dst), (imm as u32) as i32).unwrap(),
        },
        FuzzInsn::MovReg { dst, src, size } => match size {
            8 => a.mov(reg64(dst), reg64(src)).unwrap(),
            2 => a.mov(reg16(dst), reg16(src)).unwrap(),
            1 => a.mov(reg8(dst), reg8(src)).unwrap(),
            _ => a.mov(reg32(dst), reg32(src)).unwrap(),
        },
        FuzzInsn::Movzx { dst, src } => a.movzx(reg32(dst), reg8(src)).unwrap(),
        FuzzInsn::Movsx { dst, src } => a.movsx(reg32(dst), reg8(src)).unwrap(),
        FuzzInsn::Setcc { cc, dst } => setcc(a, cc, dst),
        FuzzInsn::Cmov { cc, dst, src } => cmovcc(a, cc, dst, src),
        FuzzInsn::Load { dst, off, size } => {
            let m = SCRATCH + off as u64;
            match size {
                8 => a.mov(reg64(dst), qword_ptr(m)).unwrap(),
                2 => a.mov(reg16(dst), word_ptr(m)).unwrap(),
                1 => a.mov(reg8(dst), byte_ptr(m)).unwrap(),
                _ => a.mov(reg32(dst), dword_ptr(m)).unwrap(),
            }
        }
        FuzzInsn::Store { src, off, size } => {
            let m = SCRATCH + off as u64;
            match size {
                8 => a.mov(qword_ptr(m), reg64(src)).unwrap(),
                2 => a.mov(word_ptr(m), reg16(src)).unwrap(),
                1 => a.mov(byte_ptr(m), reg8(src)).unwrap(),
                _ => a.mov(dword_ptr(m), reg32(src)).unwrap(),
            }
        }
    }
}

fn bin_reg(a: &mut CodeAssembler, op: u8, dst: u8, src: u8, size: u8) {
    macro_rules! sized {
        ($m:ident) => {
            match size {
                8 => a.$m(reg64(dst), reg64(src)).unwrap(),
                2 => a.$m(reg16(dst), reg16(src)).unwrap(),
                1 => a.$m(reg8(dst), reg8(src)).unwrap(),
                _ => a.$m(reg32(dst), reg32(src)).unwrap(),
            }
        };
    }
    match op {
        0 => sized!(add),
        1 => sized!(sub),
        2 => sized!(adc),
        3 => sized!(sbb),
        4 => sized!(and),
        5 => sized!(or),
        6 => sized!(xor),
        7 => sized!(cmp),
        _ => sized!(test),
    }
}

fn bin_imm(a: &mut CodeAssembler, op: u8, dst: u8, imm: i32, size: u8) {
    macro_rules! sized {
        ($m:ident) => {
            match size {
                8 => a.$m(reg64(dst), imm).unwrap(),
                2 => a.$m(reg16(dst), (imm as u32 & 0xffff) as i32).unwrap(),
                1 => a.$m(reg8(dst), (imm as u32 & 0xff) as i32).unwrap(),
                _ => a.$m(reg32(dst), imm).unwrap(),
            }
        };
    }
    match op {
        0 => sized!(add),
        1 => sized!(sub),
        2 => sized!(adc),
        3 => sized!(sbb),
        4 => sized!(and),
        5 => sized!(or),
        6 => sized!(xor),
        7 => sized!(cmp),
        _ => sized!(test),
    }
}

fn un_reg(a: &mut CodeAssembler, op: u8, dst: u8, size: u8) {
    macro_rules! sized {
        ($m:ident) => {
            match size {
                8 => a.$m(reg64(dst)).unwrap(),
                2 => a.$m(reg16(dst)).unwrap(),
                1 => a.$m(reg8(dst)).unwrap(),
                _ => a.$m(reg32(dst)).unwrap(),
            }
        };
    }
    match op {
        0 => sized!(inc),
        1 => sized!(dec),
        2 => sized!(neg),
        _ => sized!(not),
    }
}

fn setcc(a: &mut CodeAssembler, cc: u8, dst: u8) {
    let d = reg8(dst);
    match cc % 16 {
        0 => a.sete(d), 1 => a.setne(d), 2 => a.setb(d), 3 => a.setae(d),
        4 => a.setbe(d), 5 => a.seta(d), 6 => a.setl(d), 7 => a.setge(d),
        8 => a.setle(d), 9 => a.setg(d), 10 => a.sets(d), 11 => a.setns(d),
        12 => a.seto(d), 13 => a.setno(d), 14 => a.setp(d), _ => a.setnp(d),
    }
    .unwrap();
}

fn cmovcc(a: &mut CodeAssembler, cc: u8, dst: u8, src: u8) {
    let (d, s) = (reg32(dst), reg32(src));
    match cc % 16 {
        0 => a.cmove(d, s), 1 => a.cmovne(d, s), 2 => a.cmovb(d, s), 3 => a.cmovae(d, s),
        4 => a.cmovbe(d, s), 5 => a.cmova(d, s), 6 => a.cmovl(d, s), 7 => a.cmovge(d, s),
        8 => a.cmovle(d, s), 9 => a.cmovg(d, s), 10 => a.cmovs(d, s), 11 => a.cmovns(d, s),
        12 => a.cmovo(d, s), 13 => a.cmovno(d, s), 14 => a.cmovp(d, s), _ => a.cmovnp(d, s),
    }
    .unwrap();
}

fn reg64(i: u8) -> AsmRegister64 {
    [rax, rbx, rcx, rdx, rsi, rdi, r8, r9][i as usize]
}
fn reg32(i: u8) -> AsmRegister32 {
    [eax, ebx, ecx, edx, esi, edi, r8d, r9d][i as usize]
}
fn reg16(i: u8) -> AsmRegister16 {
    [ax, bx, cx, dx, si, di, r8w, r9w][i as usize]
}
fn reg8(i: u8) -> AsmRegister8 {
    [al, bl, cl, dl, sil, dil, r8b, r9b][i as usize]
}
