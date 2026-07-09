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
use crate::vector::FlagName::{self, *};
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
        const B: [u64; 8] = [
            0,
            1,
            u64::MAX,
            i64::MAX as u64,
            1 << 63,
            0x1234_5678,
            0xff,
            0x8000_0000,
        ];
        B[self.below(B.len())]
    }
    /// 4 or 8 — for ops without an 8/16-bit form (bt-family, bitscan, BMI, bswap).
    fn size48(&mut self) -> u8 {
        if self.next() & 1 == 0 {
            4
        } else {
            8
        }
    }
    /// 2/4/8 — 16/32/64-bit (mul/imul: the 8-bit `F6 /4` form isn't lifted yet).
    fn size248(&mut self) -> u8 {
        [2, 4, 8][self.below(3)]
    }
    /// Shift/rotate count — boundary values around every operand-width mask edge, so
    /// the count-0 no-op, count==width, and count>width cases are all hit.
    fn shift_count(&mut self) -> u8 {
        const B: [u8; 10] = [0, 1, 2, 7, 8, 15, 16, 31, 32, 63];
        B[self.below(B.len())]
    }
    /// A raw imm8 (bt bit index, pshufd/rorx selector, etc.).
    fn imm8(&mut self) -> u8 {
        self.next() as u8
    }
    /// An XMM register index (0..8) — the vector reg pool `xmm0..xmm7`.
    fn vreg(&mut self) -> u8 {
        self.below(8) as u8
    }
    /// A 128-bit seed: a lane-boundary pattern or a fully random value, so packed ops
    /// see saturating/sign edges as well as noise.
    fn vec128(&mut self) -> u128 {
        const P: [u128; 6] = [
            0,
            u128::MAX,
            0x8000_8000_8000_8000_8000_8000_8000_8000, // per-16-bit sign bits
            0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10, // ascending bytes
            0x7fff_7fff_7fff_7fff_7fff_7fff_7fff_7fff, // max signed 16-bit lanes
            0x00ff_00ff_00ff_00ff_00ff_00ff_00ff_00ff,
        ];
        match self.below(3) {
            0 => P[self.below(P.len())],
            _ => ((self.next() as u128) << 64) | self.next() as u128,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FuzzInsn {
    BinReg {
        op: u8,
        dst: u8,
        src: u8,
        size: u8,
    },
    BinImm {
        op: u8,
        dst: u8,
        imm: i32,
        size: u8,
    },
    UnReg {
        op: u8,
        dst: u8,
        size: u8,
    },
    MovImm {
        dst: u8,
        imm: u64,
        size: u8,
    },
    MovReg {
        dst: u8,
        src: u8,
        size: u8,
    },
    Movzx {
        dst: u8,
        src: u8,
    },
    Movsx {
        dst: u8,
        src: u8,
    },
    Setcc {
        cc: u8,
        dst: u8,
    },
    Cmov {
        cc: u8,
        dst: u8,
        src: u8,
    },
    Load {
        dst: u8,
        off: u16,
        size: u8,
    },
    Store {
        src: u8,
        off: u16,
        size: u8,
    },
    /// shl/shr/sar/rol/ror/rcl/rcr, by an immediate or by CL (`by_cl`).
    Shift {
        op: u8,
        dst: u8,
        size: u8,
        by_cl: bool,
        cnt: u8,
    },
    /// shld/shrd `dst, src, imm|cl` — the double-precision shift.
    DoubleShift {
        right: bool,
        dst: u8,
        src: u8,
        size: u8,
        by_cl: bool,
        cnt: u8,
    },
    /// One-operand mul/imul (implicit `RDX:RAX = RAX * src`).
    Mul1 {
        signed: bool,
        src: u8,
        size: u8,
    },
    /// Two-/three-operand imul.
    Imul2 {
        dst: u8,
        src: u8,
        size: u8,
    },
    Imul3 {
        dst: u8,
        src: u8,
        imm: i32,
        size: u8,
    },
    /// bt/bts/btr/btc `dst, imm8` — bit test (+ set/reset/complement).
    BitOp {
        op: u8,
        dst: u8,
        bit: u8,
        size: u8,
    },
    /// tzcnt/lzcnt `dst, src` (bsf/bsr omitted — their dst is undefined for a zero
    /// source, which can't be differential-compared).
    BitScan {
        op: u8,
        dst: u8,
        src: u8,
        size: u8,
    },
    Popcnt {
        dst: u8,
        src: u8,
        size: u8,
    },
    /// bswap `dst` (32/64-bit only).
    Bswap {
        dst: u8,
        size: u8,
    },
    /// BMI1/2 3-register ops: andn/blsi/blsr/blsmsk/bextr/bzhi/pdep/pext.
    Bmi {
        op: u8,
        dst: u8,
        a: u8,
        b: u8,
        size: u8,
    },
    /// shlx/shrx/sarx (count in a register) or rorx (count immediate).
    BmiShift {
        op: u8,
        dst: u8,
        src: u8,
        cnt: u8,
        size: u8,
    },
    /// mulx `hi, lo, src` (unsigned widening multiply, no flags).
    Mulx {
        hi: u8,
        lo: u8,
        src: u8,
        size: u8,
    },
    /// SSE2 packed-integer reg-reg op `xmm(dst) OP= xmm(src)` (padd*/psub*/pand/…/pack*).
    VBin {
        op: u8,
        dst: u8,
        src: u8,
    },
    /// SSE2 packed shift by an immediate: psll/psrl/psra {w,d,q}.
    VShiftImm {
        op: u8,
        dst: u8,
        imm: u8,
    },
    /// pshufd `xmm(dst), xmm(src), imm8` — cross-lane dword shuffle.
    VShuf {
        dst: u8,
        src: u8,
        imm: u8,
    },
    /// pmovmskb `reg(dst), xmm(src)` — byte-sign bitmask into a GPR.
    VMovMask {
        dst: u8,
        src: u8,
    },
}

#[derive(Clone)]
pub struct Prog {
    pub insns: Vec<FuzzInsn>,
    pub init: CpuSnapshot,
    pub seed: u64,
}

/// Flag order used by the definedness tracker and the dont-care mask.
const FLAGS: [FlagName; 6] = [Cf, Pf, Af, Zf, Sf, Of];
fn fidx(f: FlagName) -> usize {
    FLAGS.iter().position(|&x| x == f).unwrap()
}

/// Generate a random program of `len` instructions from `seed`.
///
/// A flag left *undefined* by one instruction (MUL's SF/ZF, a shift's OF, …) must
/// never be *consumed* by a later conditional (cmov/setcc/adc/sbb/rcl/rcr): the
/// consumer's result would then depend on an undefined flag and diverge from real
/// hardware for no real bug. So generation tracks per-flag definedness and re-rolls
/// any consumer whose read flags aren't all currently defined. (Flags start defined —
/// the init snapshot gives them known values.)
pub fn gen(seed: u64, len: usize) -> Prog {
    let mut rng = Rng::new(seed);
    let mut insns = Vec::with_capacity(len);
    let mut defined = [true; 6];
    for _ in 0..len {
        let insn = loop {
            let cand = gen_insn(&mut rng);
            if flag_reads(&cand).iter().all(|&f| defined[fidx(f)]) {
                break cand;
            }
        };
        let (def, und) = flag_effect(&insn);
        for f in und {
            defined[fidx(f)] = false;
        }
        for f in def {
            defined[fidx(f)] = true;
        }
        insns.push(insn);
    }
    let mut init = CpuSnapshot {
        rip: CODE,
        ..Default::default()
    };
    for &gi in &GPR_IDX {
        init.gpr[gi] = rng.imm64();
    }
    for v in 0..8 {
        init.xmm[v] = rng.vec128();
    }
    Prog { insns, init, seed }
}

/// Flags an instruction READS (a conditional consumer); empty for the rest. Used to
/// keep an undefined flag from ever reaching a consumer that turns it into an
/// observable register/flag value.
fn flag_reads(insn: &FuzzInsn) -> Vec<FlagName> {
    match *insn {
        FuzzInsn::Cmov { cc, .. } | FuzzInsn::Setcc { cc, .. } => cc_reads(cc),
        // adc/sbb (op 2/3) and rcl/rcr (shift op 5/6) read CF.
        FuzzInsn::BinReg { op: 2 | 3, .. } | FuzzInsn::BinImm { op: 2 | 3, .. } => vec![Cf],
        FuzzInsn::Shift { op: 5 | 6, .. } => vec![Cf],
        _ => vec![],
    }
}

/// The flags a condition code tests (indices match `setcc`/`cmovcc` below).
fn cc_reads(cc: u8) -> Vec<FlagName> {
    match cc % 16 {
        0 | 1 => vec![Zf],         // e/ne
        2 | 3 => vec![Cf],         // b/ae
        4 | 5 => vec![Cf, Zf],     // be/a
        6 | 7 => vec![Sf, Of],     // l/ge
        8 | 9 => vec![Zf, Sf, Of], // le/g
        10 | 11 => vec![Sf],       // s/ns
        12 | 13 => vec![Of],       // o/no
        _ => vec![Pf],             // p/np
    }
}

fn gen_insn(rng: &mut Rng) -> FuzzInsn {
    match rng.below(27) {
        0 => FuzzInsn::BinReg {
            op: rng.below(9) as u8,
            dst: rng.reg(),
            src: rng.reg(),
            size: rng.size(),
        },
        1 => FuzzInsn::BinImm {
            op: rng.below(9) as u8,
            dst: rng.reg(),
            imm: rng.imm32(),
            size: rng.size(),
        },
        2 => FuzzInsn::UnReg {
            op: rng.below(4) as u8,
            dst: rng.reg(),
            size: rng.size(),
        },
        3 => FuzzInsn::MovImm {
            dst: rng.reg(),
            imm: rng.imm64(),
            size: rng.size(),
        },
        4 => FuzzInsn::MovReg {
            dst: rng.reg(),
            src: rng.reg(),
            size: rng.size(),
        },
        5 => FuzzInsn::Movzx {
            dst: rng.reg(),
            src: rng.reg(),
        },
        6 => FuzzInsn::Movsx {
            dst: rng.reg(),
            src: rng.reg(),
        },
        7 => FuzzInsn::Setcc {
            cc: rng.below(16) as u8,
            dst: rng.reg(),
        },
        8 => FuzzInsn::Cmov {
            cc: rng.below(16) as u8,
            dst: rng.reg(),
            src: rng.reg(),
        },
        9 => FuzzInsn::Load {
            dst: rng.reg(),
            off: (rng.below(SCRATCH_LEN - 8)) as u16,
            size: rng.size(),
        },
        10 => FuzzInsn::Store {
            src: rng.reg(),
            off: (rng.below(SCRATCH_LEN - 8)) as u16,
            size: rng.size(),
        },
        11 => FuzzInsn::Shift {
            op: rng.below(7) as u8,
            dst: rng.reg(),
            size: rng.size(),
            by_cl: rng.next() & 1 == 0,
            cnt: rng.shift_count(),
        },
        12 => FuzzInsn::DoubleShift {
            right: rng.next() & 1 == 0,
            dst: rng.reg(),
            src: rng.reg(),
            size: rng.size48(), // shld/shrd: 32/64 (16-bit form omitted)
            // Immediate, always-nonzero count: Unicorn's QEMU wrongly clears the flags on
            // a shld/shrd whose count masks to 0 (verified: real hardware leaves them
            // unchanged, as does our interp), so it can't oracle that case.
            by_cl: false,
            cnt: [1, 2, 7, 8, 15, 16, 31][rng.below(7)],
        },
        13 => FuzzInsn::Mul1 {
            signed: rng.next() & 1 == 0,
            src: rng.reg(),
            size: rng.size248(),
        },
        14 => FuzzInsn::Imul2 {
            dst: rng.reg(),
            src: rng.reg(),
            size: rng.size48(),
        },
        15 => FuzzInsn::Imul3 {
            dst: rng.reg(),
            src: rng.reg(),
            imm: rng.imm32(),
            size: rng.size48(),
        },
        16 => FuzzInsn::BitOp {
            op: rng.below(4) as u8,
            dst: rng.reg(),
            bit: rng.imm8(),
            size: rng.size48(),
        },
        17 => FuzzInsn::BitScan {
            op: rng.below(2) as u8,
            dst: rng.reg(),
            src: rng.reg(),
            size: rng.size48(),
        },
        18 => FuzzInsn::Popcnt {
            dst: rng.reg(),
            src: rng.reg(),
            size: rng.size48(),
        },
        19 => FuzzInsn::Bswap {
            dst: rng.reg(),
            size: rng.size48(),
        },
        20 => FuzzInsn::Bmi {
            // BMI1 only: andn/blsi/blsr/blsmsk (0..3). The BMI2 index ops bextr/bzhi/
            // pdep/pext are omitted because Unicorn's QEMU miscomputes them — bzhi/bextr
            // clamp the index at the operand width, and pdep/pext skip the 32-bit
            // zero-extension. Both were verified wrong-in-QEMU / right-in-interp on real
            // hardware, so QEMU can't be their oracle (a NativeOracle would — task-186).
            op: rng.below(4) as u8,
            dst: rng.reg(),
            a: rng.reg(),
            b: rng.reg(),
            size: rng.size48(),
        },
        21 => FuzzInsn::BmiShift {
            op: rng.below(4) as u8,
            dst: rng.reg(),
            src: rng.reg(),
            cnt: rng.reg(),
            size: rng.size48(),
        },
        22 => FuzzInsn::Mulx {
            hi: rng.reg(),
            lo: rng.reg(),
            src: rng.reg(),
            size: rng.size48(),
        },
        23 => FuzzInsn::VBin {
            op: rng.below(V_BIN_OPS) as u8,
            dst: rng.vreg(),
            src: rng.vreg(),
        },
        24 => FuzzInsn::VShiftImm {
            op: rng.below(8) as u8,
            dst: rng.vreg(),
            imm: rng.imm8(),
        },
        25 => FuzzInsn::VShuf {
            dst: rng.vreg(),
            src: rng.vreg(),
            imm: rng.imm8(),
        },
        _ => FuzzInsn::VMovMask {
            dst: rng.reg(),
            src: rng.vreg(),
        },
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
                MemChunk {
                    addr: CODE,
                    bytes: code,
                    kind: MemKind::Ram,
                },
                MemChunk {
                    addr: SCRATCH,
                    bytes: vec![0u8; SCRATCH_LEN],
                    kind: MemKind::Ram,
                },
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
        FuzzInsn::Shift {
            op,
            dst,
            size,
            by_cl,
            cnt,
        } => shift(a, op, dst, size, by_cl, cnt),
        FuzzInsn::DoubleShift {
            right,
            dst,
            src,
            size,
            by_cl,
            cnt,
        } => double_shift(a, right, dst, src, size, by_cl, cnt),
        FuzzInsn::Mul1 { signed, src, size } => mul1(a, signed, src, size),
        FuzzInsn::Imul2 { dst, src, size } => {
            if size == 8 {
                a.imul_2(reg64(dst), reg64(src)).unwrap()
            } else {
                a.imul_2(reg32(dst), reg32(src)).unwrap()
            }
        }
        FuzzInsn::Imul3 {
            dst,
            src,
            imm,
            size,
        } => {
            if size == 8 {
                a.imul_3(reg64(dst), reg64(src), imm).unwrap()
            } else {
                a.imul_3(reg32(dst), reg32(src), imm).unwrap()
            }
        }
        FuzzInsn::BitOp { op, dst, bit, size } => bit_op(a, op, dst, bit, size),
        FuzzInsn::BitScan { op, dst, src, size } => bit_scan(a, op, dst, src, size),
        FuzzInsn::Popcnt { dst, src, size } => {
            if size == 8 {
                a.popcnt(reg64(dst), reg64(src)).unwrap()
            } else {
                a.popcnt(reg32(dst), reg32(src)).unwrap()
            }
        }
        FuzzInsn::Bswap { dst, size } => {
            if size == 8 {
                a.bswap(reg64(dst)).unwrap()
            } else {
                a.bswap(reg32(dst)).unwrap()
            }
        }
        FuzzInsn::Bmi {
            op,
            dst,
            a: ra,
            b: rb,
            size,
        } => bmi(a, op, dst, ra, rb, size),
        FuzzInsn::BmiShift {
            op,
            dst,
            src,
            cnt,
            size,
        } => bmi_shift(a, op, dst, src, cnt, size),
        FuzzInsn::Mulx { hi, lo, src, size } => {
            if size == 8 {
                a.mulx(reg64(hi), reg64(lo), reg64(src)).unwrap()
            } else {
                a.mulx(reg32(hi), reg32(lo), reg32(src)).unwrap()
            }
        }
        FuzzInsn::VBin { op, dst, src } => vbin(a, op, dst, src),
        FuzzInsn::VShiftImm { op, dst, imm } => vshift_imm(a, op, dst, imm),
        FuzzInsn::VShuf { dst, src, imm } => a.pshufd(xmm(dst), xmm(src), imm as u32).unwrap(),
        FuzzInsn::VMovMask { dst, src } => a.pmovmskb(reg32(dst), xmm(src)).unwrap(),
    }
}

/// Number of `VBin` packed-integer ops (indices into the `vbin` match below). Only
/// ops the lifter actually handles — the saturating adds (padduds*/padds*), averages
/// (pavg*), packed multiplies (pmul*/pmaddwd), and signed packs (packsswb/packssdw)
/// aren't lifted yet, so they're left out (tracked separately).
const V_BIN_OPS: usize = 29;

fn xmm(i: u8) -> AsmRegisterXmm {
    [xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7][i as usize]
}

fn vbin(a: &mut CodeAssembler, op: u8, dst: u8, src: u8) {
    let (d, s) = (xmm(dst), xmm(src));
    macro_rules! m {
        ($op:ident) => {
            a.$op(d, s).unwrap()
        };
    }
    match op {
        0 => m!(paddb),
        1 => m!(paddw),
        2 => m!(paddd),
        3 => m!(paddq),
        4 => m!(psubb),
        5 => m!(psubw),
        6 => m!(psubd),
        7 => m!(psubq),
        8 => m!(pand),
        9 => m!(por),
        10 => m!(pxor),
        11 => m!(pandn),
        12 => m!(pcmpeqb),
        13 => m!(pcmpeqw),
        14 => m!(pcmpeqd),
        15 => m!(pcmpgtb),
        16 => m!(pcmpgtw),
        17 => m!(pcmpgtd),
        18 => m!(punpcklbw),
        19 => m!(punpcklwd),
        20 => m!(punpckldq),
        21 => m!(punpcklqdq),
        22 => m!(punpckhbw),
        23 => m!(punpckhwd),
        24 => m!(punpckhdq),
        25 => m!(punpckhqdq),
        26 => m!(packuswb),
        27 => m!(pminub),
        _ => m!(pmaxub),
    }
}

fn vshift_imm(a: &mut CodeAssembler, op: u8, dst: u8, imm: u8) {
    let d = xmm(dst);
    let i = imm as u32;
    match op % 8 {
        0 => a.psllw(d, i),
        1 => a.pslld(d, i),
        2 => a.psllq(d, i),
        3 => a.psrlw(d, i),
        4 => a.psrld(d, i),
        5 => a.psrlq(d, i),
        6 => a.psraw(d, i),
        _ => a.psrad(d, i),
    }
    .unwrap();
}

fn shift(a: &mut CodeAssembler, op: u8, dst: u8, size: u8, by_cl: bool, cnt: u8) {
    // A shift/rotate by CL (variable) or by an immediate count. iced wants the count
    // register `cl` or a u32 immediate; the guest masks it to 5/6 bits.
    macro_rules! by {
        ($m:ident) => {{
            if by_cl {
                match size {
                    8 => a.$m(reg64(dst), cl).unwrap(),
                    2 => a.$m(reg16(dst), cl).unwrap(),
                    1 => a.$m(reg8(dst), cl).unwrap(),
                    _ => a.$m(reg32(dst), cl).unwrap(),
                }
            } else {
                let c = cnt as u32;
                match size {
                    8 => a.$m(reg64(dst), c).unwrap(),
                    2 => a.$m(reg16(dst), c).unwrap(),
                    1 => a.$m(reg8(dst), c).unwrap(),
                    _ => a.$m(reg32(dst), c).unwrap(),
                }
            }
        }};
    }
    match op {
        0 => by!(shl),
        1 => by!(shr),
        2 => by!(sar),
        3 => by!(rol),
        4 => by!(ror),
        5 => by!(rcl),
        _ => by!(rcr),
    }
}

fn double_shift(
    a: &mut CodeAssembler,
    right: bool,
    dst: u8,
    src: u8,
    size: u8,
    by_cl: bool,
    cnt: u8,
) {
    macro_rules! by {
        ($m:ident) => {{
            if by_cl {
                if size == 8 {
                    a.$m(reg64(dst), reg64(src), cl).unwrap()
                } else {
                    a.$m(reg32(dst), reg32(src), cl).unwrap()
                }
            } else {
                let c = cnt as u32;
                if size == 8 {
                    a.$m(reg64(dst), reg64(src), c).unwrap()
                } else {
                    a.$m(reg32(dst), reg32(src), c).unwrap()
                }
            }
        }};
    }
    if right {
        by!(shrd)
    } else {
        by!(shld)
    }
}

fn mul1(a: &mut CodeAssembler, signed: bool, src: u8, size: u8) {
    macro_rules! sized {
        ($m:ident) => {
            match size {
                8 => a.$m(reg64(src)).unwrap(),
                2 => a.$m(reg16(src)).unwrap(),
                1 => a.$m(reg8(src)).unwrap(),
                _ => a.$m(reg32(src)).unwrap(),
            }
        };
    }
    if signed {
        sized!(imul)
    } else {
        sized!(mul)
    }
}

fn bit_op(a: &mut CodeAssembler, op: u8, dst: u8, bit: u8, size: u8) {
    let b = bit as u32; // bt masks the index to the operand width internally
    macro_rules! sized {
        ($m:ident) => {
            if size == 8 {
                a.$m(reg64(dst), b).unwrap()
            } else {
                a.$m(reg32(dst), b).unwrap()
            }
        };
    }
    match op {
        0 => sized!(bt),
        1 => sized!(bts),
        2 => sized!(btr),
        _ => sized!(btc),
    }
}

fn bit_scan(a: &mut CodeAssembler, op: u8, dst: u8, src: u8, size: u8) {
    macro_rules! sized {
        ($m:ident) => {
            if size == 8 {
                a.$m(reg64(dst), reg64(src)).unwrap()
            } else {
                a.$m(reg32(dst), reg32(src)).unwrap()
            }
        };
    }
    match op {
        0 => sized!(tzcnt),
        _ => sized!(lzcnt),
    }
}

fn bmi(a: &mut CodeAssembler, op: u8, dst: u8, ra: u8, rb: u8, size: u8) {
    macro_rules! sized {
        ($m:ident) => {
            if size == 8 {
                a.$m(reg64(dst), reg64(ra), reg64(rb)).unwrap()
            } else {
                a.$m(reg32(dst), reg32(ra), reg32(rb)).unwrap()
            }
        };
    }
    // Single-source ops (blsi/blsr/blsmsk) ignore `rb`.
    macro_rules! sized1 {
        ($m:ident) => {
            if size == 8 {
                a.$m(reg64(dst), reg64(ra)).unwrap()
            } else {
                a.$m(reg32(dst), reg32(ra)).unwrap()
            }
        };
    }
    match op {
        0 => sized!(andn),
        1 => sized1!(blsi),
        2 => sized1!(blsr),
        3 => sized1!(blsmsk),
        4 => sized!(bextr),
        5 => sized!(bzhi),
        6 => sized!(pdep),
        _ => sized!(pext),
    }
}

fn bmi_shift(a: &mut CodeAssembler, op: u8, dst: u8, src: u8, cnt: u8, size: u8) {
    macro_rules! sized {
        ($m:ident) => {
            if size == 8 {
                a.$m(reg64(dst), reg64(src), reg64(cnt)).unwrap()
            } else {
                a.$m(reg32(dst), reg32(src), reg32(cnt)).unwrap()
            }
        };
    }
    match op {
        0 => sized!(shlx),
        1 => sized!(shrx),
        2 => sized!(sarx),
        _ => {
            // rorx takes an immediate rotate, not a count register.
            let imm = (cnt as u32) & 0x3f;
            if size == 8 {
                a.rorx(reg64(dst), reg64(src), imm).unwrap()
            } else {
                a.rorx(reg32(dst), reg32(src), imm).unwrap()
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
        0 => a.sete(d),
        1 => a.setne(d),
        2 => a.setb(d),
        3 => a.setae(d),
        4 => a.setbe(d),
        5 => a.seta(d),
        6 => a.setl(d),
        7 => a.setge(d),
        8 => a.setle(d),
        9 => a.setg(d),
        10 => a.sets(d),
        11 => a.setns(d),
        12 => a.seto(d),
        13 => a.setno(d),
        14 => a.setp(d),
        _ => a.setnp(d),
    }
    .unwrap();
}

fn cmovcc(a: &mut CodeAssembler, cc: u8, dst: u8, src: u8) {
    let (d, s) = (reg32(dst), reg32(src));
    match cc % 16 {
        0 => a.cmove(d, s),
        1 => a.cmovne(d, s),
        2 => a.cmovb(d, s),
        3 => a.cmovae(d, s),
        4 => a.cmovbe(d, s),
        5 => a.cmova(d, s),
        6 => a.cmovl(d, s),
        7 => a.cmovge(d, s),
        8 => a.cmovle(d, s),
        9 => a.cmovg(d, s),
        10 => a.cmovs(d, s),
        11 => a.cmovns(d, s),
        12 => a.cmovo(d, s),
        13 => a.cmovno(d, s),
        14 => a.cmovp(d, s),
        _ => a.cmovnp(d, s),
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

// --- undefined-flag model (for the differential-vs-Unicorn comparison) ---
//
// Many of these instructions leave some arithmetic flags *architecturally undefined*
// (MUL: SF/ZF/AF/PF; a shift by count≠1: OF; bt: everything but CF; …). The interpreter
// and the JIT agree on whatever value they compute (so `jit == interp` stays exact), but
// real hardware (Unicorn) is free to differ — so the differential must ignore any flag
// that is undefined in the program's final state.

/// `(flags this instruction DEFINES, flags it leaves UNDEFINED)`. A flag in neither is
/// untouched (keeps its prior value).
fn flag_effect(insn: &FuzzInsn) -> (Vec<FlagName>, Vec<FlagName>) {
    let all = || vec![Cf, Of, Sf, Zf, Af, Pf];
    match *insn {
        // op 0..3 = add/sub/adc/sbb, 7 = cmp → all defined; 4..6/8 = and/or/xor/test → AF undef.
        FuzzInsn::BinReg { op, .. } | FuzzInsn::BinImm { op, .. } => {
            if op <= 3 || op == 7 {
                (all(), vec![])
            } else {
                (vec![Cf, Of, Sf, Zf, Pf], vec![Af])
            }
        }
        FuzzInsn::UnReg { op, .. } => match op {
            0 | 1 => (vec![Of, Sf, Zf, Af, Pf], vec![]), // inc/dec leave CF untouched
            2 => (all(), vec![]),                        // neg
            _ => (vec![], vec![]),                       // not
        },
        FuzzInsn::Mul1 { .. } | FuzzInsn::Imul2 { .. } | FuzzInsn::Imul3 { .. } => {
            (vec![Cf, Of], vec![Sf, Zf, Af, Pf])
        }
        FuzzInsn::Shift {
            op,
            size,
            by_cl,
            cnt,
            ..
        } => shift_flags(op, size, by_cl, cnt),
        FuzzInsn::DoubleShift {
            size, by_cl, cnt, ..
        } => {
            if by_cl {
                (vec![], all()) // dynamic count (may be 0): mask conservatively
            } else if (cnt as u32) & (size as u32 * 8 - 1) == 0 {
                (vec![], vec![]) // effective count 0 → untouched
            } else {
                (vec![Cf, Sf, Zf, Pf], vec![Of, Af])
            }
        }
        FuzzInsn::BitOp { .. } => (vec![Cf], vec![Of, Sf, Zf, Af, Pf]), // bt*: only CF defined
        FuzzInsn::BitScan { .. } => (vec![Cf, Zf], vec![Of, Sf, Af, Pf]), // tzcnt/lzcnt
        FuzzInsn::Popcnt { .. } => (all(), vec![]), // ZF per result, rest cleared
        FuzzInsn::Bmi { op, .. } => match op {
            6 | 7 => (vec![], vec![]), // pdep/pext touch no flags
            _ => (vec![], all()), // andn/bls*/bextr/bzhi: partly-undefined → mask conservatively
        },
        // mulx/rorx/shlx/shrx/sarx, mov*, setcc, cmov, load/store, bswap: no flags.
        _ => (vec![], vec![]),
    }
}

/// Flag effect of a shift/rotate. The count is masked to the operand width; a masked
/// count of 0 touches nothing, OF is defined only for a count of exactly 1, and a
/// by-CL (dynamic) count is masked conservatively.
fn shift_flags(op: u8, size: u8, by_cl: bool, cnt: u8) -> (Vec<FlagName>, Vec<FlagName>) {
    let rotate = op >= 3; // 3..6 = rol/ror/rcl/rcr — affect only CF and OF
    if by_cl {
        return if rotate {
            (vec![], vec![Cf, Of])
        } else {
            (vec![], vec![Cf, Of, Sf, Zf, Af, Pf])
        };
    }
    let eff = (cnt as u32) & (size as u32 * 8 - 1);
    if eff == 0 {
        return (vec![], vec![]);
    }
    let of_def = eff == 1;
    if rotate {
        if of_def {
            (vec![Cf, Of], vec![])
        } else {
            (vec![Cf], vec![Of])
        }
    } else if of_def {
        (vec![Cf, Of, Sf, Zf, Pf], vec![Af])
    } else {
        (vec![Cf, Sf, Zf, Pf], vec![Of, Af])
    }
}

/// Flags that are architecturally UNDEFINED in the program's final state — the last
/// instruction to write each flag decides. Mask these when comparing against Unicorn.
pub fn dontcare_flags(prog: &Prog) -> Vec<FlagName> {
    let mut undef = [false; 6];
    for insn in &prog.insns {
        let (def, und) = flag_effect(insn);
        for f in und {
            undef[fidx(f)] = true;
        }
        for f in def {
            undef[fidx(f)] = false;
        }
    }
    FLAGS.iter().copied().filter(|&f| undef[fidx(f)]).collect()
}
