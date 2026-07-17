//! Differential fuzzing (testing.md §7): generate random *valid* programs from
//! the supported instruction set, run them through two engines, and any state
//! divergence is a bug. Programs are structured (a `Vec<FuzzInsn>`) so a
//! divergence can be shrunk (§7.2) to a minimal reproducer, and the whole thing
//! is seed-deterministic (§7.3).
//!
//! Only pure computation — no syscalls/MMIO/branches to unmapped code — so runs
//! are reproducible. Memory operands are confined to a mapped scratch region.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced_x86::code_asm::*;
use x86jit_core::{CpuMode, GuestCpuFeatures, InterpreterBackend};
use x86jit_cranelift::JitBackend;

use crate::compare::compare_nan_tolerant as compare;
use crate::oracle::{run_with_backend_mode, RunOutcome, VectorInput};
use crate::vector::FlagName::{self, *};
use crate::vector::{CpuSnapshot, MemChunk, MemKind, RunSpec};

// Guest layout. Kept above `mmap_min_addr` (0x10000) and clear of the NativeOracle's
// control window (0x200000..0x203000, native.rs) so the fuzzer's programs can also be
// executed on the real host CPU. Engines are address-agnostic; only native cares.
const CODE: u64 = 0x21_0000;
pub const SCRATCH: u64 = 0x22_0000;
const SCRATCH_LEN: usize = 0x1000;

/// Register pool (avoids RSP/RBP so a stray write can't wreck addressing). Index
/// `i` maps to `gpr[GPR_IDX[i]]` in the snapshot.
const GPR_IDX: [usize; 8] = [0, 3, 1, 2, 6, 7, 8, 9];
const POOL: usize = 8;
/// 32-bit register pool size: the first 4 pool entries (rax,rbx,rcx,rdx). Excludes
/// r8/r9 (need REX) and rsi/rdi, whose 8-bit forms (sil/dil) also need REX — legacy
/// 32-bit encoding only exposes al/bl/cl/dl as byte registers, so restricting to
/// these four keeps every operand byte/word/dword-addressable in any size.
const POOL32: usize = 4;

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
    /// 32-bit register pool: only the 6 legacy GPRs the mode has (rax,rbx,rcx,rdx,
    /// rsi,rdi — pool indices 0..6); r8/r9 (indices 6,7) don't exist without REX.
    fn reg32(&mut self) -> u8 {
        self.below(POOL32) as u8
    }
    fn size(&mut self) -> u8 {
        [4, 8, 4, 8, 1, 2][self.below(6)]
    }
    /// Operand size for 32-bit mode: 8/16/32 only (no 64-bit operands).
    fn size_compat32(&mut self) -> u8 {
        [4, 4, 1, 2][self.below(4)]
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
    /// 1/2/4/8 — 8/16/32/64-bit. The 8-bit one-operand `mul`/`imul` (`F6 /4,/5`) is now
    /// lifted (task-189), so size 1 is back in the menu.
    fn size1248(&mut self) -> u8 {
        [1, 2, 4, 8][self.below(4)]
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

    /// A 128-bit seed biased toward FP corner values (task-268), for register init on the
    /// AVX **float** path only. Float ops (convert, fma, float-horizontal, dpps, round) have
    /// their sharp edges exactly at signed zero/inf, quiet+signalling NaN, the subnormal and
    /// smallest/largest-normal boundaries, the f16 overflow/underflow edges (vcvtps2ph), and
    /// half-ulp rounding straddles — values uniform random bits almost never produce. Picks a
    /// lane width (f16/f32/f64) and fills each lane from that width's corner set, leaving ~1
    /// lane in 4 random so both all-corner and corner-in-noise vectors occur; 1 draw in 4
    /// falls back to [`Rng::vec128`] so integer/lane ops in a mixed program keep their
    /// adversarial byte patterns. NOT used on the non-AVX path — see [`gen_mode_ops`] — so the
    /// `gen`/`gen32` RNG streams stay byte-identical.
    fn vec128_fp(&mut self) -> u128 {
        match self.below(4) {
            0 => self.vec128(),
            1 => self.pack_fp16(),
            2 => self.pack_fp32(),
            _ => self.pack_fp64(),
        }
    }
    /// One f16 lane: an FP corner (~3 in 4) or random bits (corner-in-noise).
    fn fp_lane16(&mut self) -> u16 {
        // +0 -0 +inf -inf qNaN sNaN min-subnormal max-subnormal min-normal max-normal(65504)
        // 1.0 -1.0 0.5 (½-ulp straddle) largest-below-0.5.
        const C: [u16; 14] = [
            0x0000, 0x8000, 0x7c00, 0xfc00, 0x7e00, 0x7c01, 0x0001, 0x03ff, 0x0400, 0x7bff, 0x3c00,
            0xbc00, 0x3800, 0x37ff,
        ];
        if self.below(4) == 0 {
            self.next() as u16
        } else {
            C[self.below(C.len())]
        }
    }
    /// One f32 lane: an FP corner (~3 in 4) or random bits.
    fn fp_lane32(&mut self) -> u32 {
        // +0 -0 +inf -inf qNaN sNaN min-subnormal max-subnormal min-normal max-normal 1.0 -1.0
        // 65504.0 (largest f16, cvtps2ph edge) 65520.0 (round-to-inf midpoint) 2^-14 (smallest
        // f16 normal) 2^-24 (smallest f16 subnormal) 2^-25 (½ smallest subnormal → underflow)
        // 1.5 1.0+1ulp (½-ulp straddles).
        const C: [u32; 19] = [
            0x0000_0000,
            0x8000_0000,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_0000,
            0x7f80_0001,
            0x0000_0001,
            0x007f_ffff,
            0x0080_0000,
            0x7f7f_ffff,
            0x3f80_0000,
            0xbf80_0000,
            0x477f_e000,
            0x477f_f000,
            0x3880_0000,
            0x3380_0000,
            0x3300_0000,
            0x3fc0_0000,
            0x3f80_0001,
        ];
        if self.below(4) == 0 {
            self.next() as u32
        } else {
            C[self.below(C.len())]
        }
    }
    /// One f64 lane: an FP corner (~3 in 4) or random bits.
    fn fp_lane64(&mut self) -> u64 {
        // +0 -0 +inf -inf qNaN sNaN min-subnormal max-subnormal min-normal max-normal 1.0 -1.0
        // 1.5 1.0+1ulp (½-ulp straddles) 100.0 (plain normal).
        const C: [u64; 15] = [
            0x0000_0000_0000_0000,
            0x8000_0000_0000_0000,
            0x7ff0_0000_0000_0000,
            0xfff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
            0x7ff0_0000_0000_0001,
            0x0000_0000_0000_0001,
            0x000f_ffff_ffff_ffff,
            0x0010_0000_0000_0000,
            0x7fef_ffff_ffff_ffff,
            0x3ff0_0000_0000_0000,
            0xbff0_0000_0000_0000,
            0x3ff8_0000_0000_0000,
            0x3ff0_0000_0000_0001,
            0x4059_0000_0000_0000,
        ];
        if self.below(4) == 0 {
            self.next()
        } else {
            C[self.below(C.len())]
        }
    }
    /// Pack 8 f16 corner lanes into a 128-bit vector.
    fn pack_fp16(&mut self) -> u128 {
        let mut v = 0u128;
        for i in 0..8 {
            v |= (self.fp_lane16() as u128) << (i * 16);
        }
        v
    }
    /// Pack 4 f32 corner lanes into a 128-bit vector.
    fn pack_fp32(&mut self) -> u128 {
        let mut v = 0u128;
        for i in 0..4 {
            v |= (self.fp_lane32() as u128) << (i * 32);
        }
        v
    }
    /// Pack 2 f64 corner lanes into a 128-bit vector.
    fn pack_fp64(&mut self) -> u128 {
        let mut v = 0u128;
        for i in 0..2 {
            v |= (self.fp_lane64() as u128) << (i * 64);
        }
        v
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
    /// Legacy-SSE forms of the SSE3/SSSE3/SSE4.1 ops lifted in task-242..249:
    /// round{ps,pd,ss,sd}, h{add,sub}p{s,d}, addsubp{s,d}, ph{add,sub}{w,d,sw},
    /// psadbw. Register source only (memory forms are exercised elsewhere). Legacy
    /// (not VEX) encoding so every oracle — interpreter, JIT, Unicorn, and the real
    /// host CPU — decodes them identically; the VEX forms share the same compute path.
    VNew {
        op: u8,
        dst: u8,
        src: u8,
    },
    /// pmovmskb `reg(dst), xmm(src)` — byte-sign bitmask into a GPR.
    VMovMask {
        dst: u8,
        src: u8,
    },
    /// AVX/AVX2 VEX-encoded vector ops from the task-259..264 sweep (vmaskmov, packed-int
    /// sat/avg/minmax/mulhrsw/pmadd, float horizontal + FMA add-sub, blends, permute/
    /// shuffle/byte-shift/dup, width converts, dpps, round, mpsadbw, phminposuw). All
    /// forms are vector-in/vector-out (no flag/GPR results), 3-/4-operand where the family
    /// requires it, exercised at ymm width (upper 128 seeded). The NativeOracle decodes
    /// VEX correctly, so the real host CPU is the ground truth; the JIT-vs-interp leg
    /// covers codegen. `op` indexes the `vvex` table; `imm` feeds the imm-control forms.
    VVex {
        op: u8,
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
    },
}

#[derive(Clone)]
pub struct Prog {
    pub insns: Vec<FuzzInsn>,
    pub init: CpuSnapshot,
    pub seed: u64,
    /// Guest mode the program is assembled and executed under (task-197). `Long64`
    /// is the historical default; `Compat32` drives the 32-bit differential lane.
    pub mode: CpuMode,
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
    gen_mode(seed, len, CpuMode::Long64, false)
}

/// Like [`gen`] but the instruction pool also includes the AVX2 VEX ops (`FuzzInsn::VVex`,
/// task-264) and the ymm upper halves are seeded. Kept SEPARATE from `gen` because those
/// ops legitimately diverge on unspecified NaN sign/payload (native vs softfloat) and are
/// VEX-encoded (Unicorn's QEMU mis-decodes them) — so they must not pollute the general
/// differential fuzz legs. Only the dedicated `fuzz_avx` driver, whose oracles tolerate
/// that noise, uses this generator.
pub fn gen_avx(seed: u64, len: usize) -> Prog {
    gen_mode(seed, len, CpuMode::Long64, true)
}

/// True if `prog` contains an instruction Unicorn's QEMU build cannot oracle. The
/// SSSE3 packed-integer horizontal add/sub family (`ph{add,sub}{w,d,sw}`, `VNew` op
/// indices 10..=15) is mis-decoded by that QEMU (it returns zero — verified: the
/// interpreter matches the real host CPU via the NativeOracle, so *interp* is right
/// and QEMU is wrong), so the Unicorn differential must skip these — the NativeOracle
/// and the JIT-vs-interp legs cover them. Same rationale as the omitted BMI2 index ops.
pub fn unicorn_incompatible(prog: &Prog) -> bool {
    prog.insns
        .iter()
        .any(|i| matches!(i, FuzzInsn::VNew { op, .. } if (10..=15).contains(op)))
}

/// Generate a random 32-bit (`CpuMode::Compat32`) program (task-197): the mode-A
/// fuzz lane. Same generator, restricted to instruction forms whose *encoding* is
/// mode-neutral or genuinely 32-bit — 8-bit/16-bit/32-bit operands only (no 64-bit),
/// the 6-register legacy pool (no r8–r15, no REX), and the 0x40–0x4F `inc`/`dec`
/// short forms that a 32-bit assembler emits for `UnReg` inc/dec.
pub fn gen32(seed: u64, len: usize) -> Prog {
    gen_mode(seed, len, CpuMode::Compat32, false)
}

/// Shared generator body; `mode` selects the 64-bit or 32-bit instruction envelope, `avx`
/// adds the VEX/AVX2 `VVex` pool (Long64 only) and seeds the ymm upper halves. Uses the
/// full [`V_VEX`] pool for VEX-op selection; see [`gen_mode_ops`] to subset it.
pub fn gen_mode(seed: u64, len: usize, mode: CpuMode, avx: bool) -> Prog {
    gen_mode_ops(seed, len, mode, avx, None)
}

/// Like [`gen_mode`] but `vex_ops`, when `Some`, restricts `VVex` op selection to that
/// subset of [`V_VEX`] indices (the `--ops`/`--family` CLI knobs, task-267). `None` draws
/// from the whole pool. Passing `None` — or a `Some` slice equal to the full index range in
/// order — yields a byte-identical RNG stream to the historical generator, so existing seeds
/// keep their meaning (only the VEX-op *index* is remapped through the subset).
pub fn gen_mode_ops(
    seed: u64,
    len: usize,
    mode: CpuMode,
    avx: bool,
    vex_ops: Option<&[usize]>,
) -> Prog {
    let mut rng = Rng::new(seed);
    let mut insns = Vec::with_capacity(len);
    let mut defined = [true; 6];
    for _ in 0..len {
        let insn = loop {
            let cand = gen_insn_mode(&mut rng, mode, avx, vex_ops);
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
    // In 32-bit mode only the 6-register legacy pool exists; leaving r8–r15 at zero
    // keeps them matching Unicorn's UC_MODE_32 (which has no such registers), and the
    // init values are truncated to 32 bits since a 32-bit guest can't hold more.
    for &gi in &GPR_IDX {
        if mode == CpuMode::Compat32 && gi >= 8 {
            continue;
        }
        init.gpr[gi] = match mode {
            CpuMode::Compat32 => rng.imm64() & 0xffff_ffff,
            CpuMode::Long64 => rng.imm64(),
            CpuMode::Real16 => unreachable!("fuzz harness does not target Real16"),
        };
    }
    // Bias operands toward FP corner values ONLY for AVX programs that contain a float VEX op
    // (task-268): those ops (convert/fma/float-horizontal/dpps/round) have their sharp edges at
    // FP special values. The non-AVX path (`gen`/`gen32`) and integer-only AVX programs keep
    // `vec128`, so their RNG streams stay byte-identical — the differential fuzz tests depend on
    // that. `vec128_fp` still mixes in integer/random lanes, so integer/lane ops in a mixed
    // float program keep their adversarial byte patterns.
    let float_avx = avx
        && insns
            .iter()
            .any(|i| matches!(i, FuzzInsn::VVex { op, .. } if V_VEX[*op as usize].is_float()));
    for v in 0..8 {
        init.xmm[v] = if float_avx {
            rng.vec128_fp()
        } else {
            rng.vec128()
        };
    }
    // Seed the ymm upper halves only in the AVX fuzz lane with a VEX op present, so the general
    // differential legs keep their historical all-zero-upper init.
    if avx && insns.iter().any(|i| matches!(i, FuzzInsn::VVex { .. })) {
        for v in 0..8 {
            init.ymm_hi[v] = if float_avx {
                rng.vec128_fp()
            } else {
                rng.vec128()
            };
        }
    }
    Prog {
        insns,
        init,
        seed,
        mode,
    }
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

/// Pick a random instruction for the given guest mode. `Long64` uses the full
/// envelope; `Compat32` uses [`gen_insn32`], a restricted set whose encodings are
/// mode-neutral or genuinely 32-bit (no 64-bit operands, no r8–r15).
fn gen_insn_mode(rng: &mut Rng, mode: CpuMode, avx: bool, vex_ops: Option<&[usize]>) -> FuzzInsn {
    match mode {
        CpuMode::Long64 => gen_insn(rng, avx, vex_ops),
        CpuMode::Compat32 => gen_insn32(rng),
        CpuMode::Real16 => unreachable!("fuzz harness does not target Real16"),
    }
}

/// The 32-bit (`CpuMode::Compat32`) instruction generator. Reuses the same
/// `FuzzInsn` shapes as the 64-bit path but only emits forms a 32-bit guest can
/// encode: 8/16/32-bit operands, the 6-register legacy pool, and — crucially — the
/// `inc`/`dec` `UnReg` forms, which a 32-bit assembler encodes as the single-byte
/// 0x40–0x4F opcodes (REX prefixes in long mode). Vector (SSE) ops are mode-neutral
/// so they're kept; BMI/64-bit-only widening ops are dropped.
fn gen_insn32(rng: &mut Rng) -> FuzzInsn {
    match rng.below(15) {
        0 => FuzzInsn::BinReg {
            op: rng.below(9) as u8,
            dst: rng.reg32(),
            src: rng.reg32(),
            size: rng.size_compat32(),
        },
        1 => FuzzInsn::BinImm {
            op: rng.below(9) as u8,
            dst: rng.reg32(),
            imm: rng.imm32(),
            size: rng.size_compat32(),
        },
        2 => FuzzInsn::UnReg {
            // inc/dec (0/1) are the 0x40–0x4F short forms in 32-bit; neg/not (2/3) too.
            op: rng.below(4) as u8,
            dst: rng.reg32(),
            size: rng.size_compat32(),
        },
        3 => FuzzInsn::MovImm {
            dst: rng.reg32(),
            imm: rng.imm64() & 0xffff_ffff,
            size: rng.size_compat32(),
        },
        4 => FuzzInsn::MovReg {
            dst: rng.reg32(),
            src: rng.reg32(),
            size: rng.size_compat32(),
        },
        5 => FuzzInsn::Movzx {
            dst: rng.reg32(),
            src: rng.reg32(),
        },
        6 => FuzzInsn::Movsx {
            dst: rng.reg32(),
            src: rng.reg32(),
        },
        7 => FuzzInsn::Setcc {
            cc: rng.below(16) as u8,
            dst: rng.reg32(),
        },
        8 => FuzzInsn::Cmov {
            cc: rng.below(16) as u8,
            dst: rng.reg32(),
            src: rng.reg32(),
        },
        9 => FuzzInsn::Load {
            dst: rng.reg32(),
            off: (rng.below(SCRATCH_LEN - 8)) as u16,
            size: rng.size_compat32(),
        },
        10 => FuzzInsn::Store {
            src: rng.reg32(),
            off: (rng.below(SCRATCH_LEN - 8)) as u16,
            size: rng.size_compat32(),
        },
        11 => FuzzInsn::Shift {
            op: rng.below(7) as u8,
            dst: rng.reg32(),
            size: rng.size_compat32(),
            by_cl: rng.next() & 1 == 0,
            cnt: rng.shift_count(),
        },
        12 => FuzzInsn::Mul1 {
            signed: rng.next() & 1 == 0,
            src: rng.reg32(),
            size: [1, 2, 4][rng.below(3)], // 8/16/32-bit (8-bit F6 /4,/5 lifted, task-189; no 64-bit here)
        },
        13 => FuzzInsn::Imul2 {
            dst: rng.reg32(),
            src: rng.reg32(),
            size: 4, // 32-bit only
        },
        _ => FuzzInsn::Imul3 {
            dst: rng.reg32(),
            src: rng.reg32(),
            imm: rng.imm32(),
            size: 4,
        },
    }
}

fn gen_insn(rng: &mut Rng, avx: bool, vex_ops: Option<&[usize]>) -> FuzzInsn {
    // 28 base variants (0..=27); the AVX lane adds a 29th (VVex) as the catch-all.
    match rng.below(if avx { 29 } else { 28 }) {
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
            size: rng.size1248(),
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
        26 => FuzzInsn::VNew {
            op: rng.below(V_NEW_OPS) as u8,
            dst: rng.vreg(),
            src: rng.vreg(),
        },
        27 => FuzzInsn::VMovMask {
            dst: rng.reg(),
            src: rng.vreg(),
        },
        _ => FuzzInsn::VVex {
            // Draw a VEX-op index. `None` → the whole pool (`rng.below(V_VEX.len())`, the
            // historical draw, kept byte-identical); `Some(sub)` → an index from the subset,
            // remapped through it. A `Some` slice equal to `0..V_VEX.len()` in order is
            // indistinguishable from `None`, so the default campaign preserves the stream.
            op: match vex_ops {
                None => rng.below(V_VEX.len()) as u8,
                Some(sub) => sub[rng.below(sub.len())] as u8,
            },
            dst: rng.vreg(),
            a: rng.vreg(),
            b: rng.vreg(),
            imm: rng.imm8(),
        },
    }
}

impl Prog {
    /// Assemble to a runnable input (append `hlt`; map code + a scratch region).
    /// The assembler bitness follows `self.mode`, so a `Compat32` program encodes
    /// its `inc`/`dec` as the 0x40–0x4F short forms and uses 32-bit addressing.
    pub fn input(&self) -> VectorInput {
        let bitness = match self.mode {
            CpuMode::Long64 => 64,
            CpuMode::Compat32 => 32,
            CpuMode::Real16 => unreachable!("fuzz harness does not target Real16"),
        };
        let mut a = CodeAssembler::new(bitness).unwrap();
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
        FuzzInsn::VNew { op, dst, src } => vnew(a, op, dst, src),
        FuzzInsn::VShiftImm { op, dst, imm } => vshift_imm(a, op, dst, imm),
        FuzzInsn::VShuf { dst, src, imm } => a.pshufd(xmm(dst), xmm(src), imm as u32).unwrap(),
        FuzzInsn::VMovMask { dst, src } => a.pmovmskb(reg32(dst), xmm(src)).unwrap(),
        FuzzInsn::VVex {
            op,
            dst,
            a: aa,
            b,
            imm,
        } => vvex(a, op, dst, aa, b, imm),
    }
}

/// Number of `VBin` packed-integer ops (indices into the `vbin` match below). All
/// ops the lifter handles, including the SSE2 saturating adds/subs (padds*/paddus*/
/// psubs*/psubus*), rounding averages (pavg*), signed packs (packsswb/packssdw) and
/// the multiply-add pmaddwd (task-190).
const V_BIN_OPS: usize = 42;

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
        28 => m!(pmaxub),
        // SSE2 saturating add/sub, rounding average, signed packs, pmaddwd (task-190).
        29 => m!(paddsb),
        30 => m!(paddsw),
        31 => m!(paddusb),
        32 => m!(paddusw),
        33 => m!(psubsb),
        34 => m!(psubsw),
        35 => m!(psubusb),
        36 => m!(psubusw),
        37 => m!(pavgb),
        38 => m!(pavgw),
        39 => m!(packsswb),
        40 => m!(packssdw),
        _ => m!(pmaddwd),
    }
}

/// Number of `VNew` register-form ops (indices into the `vnew` match below).
const V_NEW_OPS: usize = 20;

fn vnew(a: &mut CodeAssembler, op: u8, dst: u8, src: u8) {
    let (d, s) = (xmm(dst), xmm(src));
    macro_rules! m {
        ($op:ident) => {
            a.$op(d, s).unwrap()
        };
    }
    match op % V_NEW_OPS as u8 {
        // round{ps,pd,ss,sd} with a representative rounding mode each (task-242).
        0 => a.roundps(d, s, 0u32).unwrap(), // nearest
        1 => a.roundpd(d, s, 1u32).unwrap(), // floor
        2 => a.roundss(d, s, 2u32).unwrap(), // ceil
        3 => a.roundsd(d, s, 3u32).unwrap(), // trunc
        // horizontal + addsub packed float (task-244).
        4 => m!(haddps),
        5 => m!(haddpd),
        6 => m!(hsubps),
        7 => m!(hsubpd),
        8 => m!(addsubps),
        9 => m!(addsubpd),
        // integer horizontal add/sub (task-247).
        10 => m!(phaddw),
        11 => m!(phaddd),
        12 => m!(phaddsw),
        13 => m!(phsubw),
        14 => m!(phsubd),
        15 => m!(phsubsw),
        // sum-of-absolute-differences (task-249).
        16 => m!(psadbw),
        // register-source unpack/pack that also route through the task-243 paths.
        17 => m!(punpcklqdq),
        18 => m!(packssdw),
        _ => m!(packsswb),
    }
}

fn ymm(i: u8) -> AsmRegisterYmm {
    [ymm0, ymm1, ymm2, ymm3, ymm4, ymm5, ymm6, ymm7][i as usize]
}

/// Family a [`VexOp`] belongs to — the `--family` selector and the coverage grouping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    /// Packed-integer saturating add/sub, averages, min/max, mulhrsw, madd.
    PackedInt,
    /// Integer horizontal add/sub (+ saturating) and sign.
    HorizontalInt,
    /// Float horizontal add/sub and addsub.
    FloatHorizontal,
    /// Lane permutes (vpermilps/pd variable, vpermps).
    Permute,
    /// Variable- and immediate-controlled blends, mpsadbw, dpps.
    Blend,
    /// Immediate 2-operand shuffles, byte-shifts, round, permil-imm.
    Shuffle,
    /// Lane-duplicating moves (vmovddup/shdup/sldup).
    Dup,
    /// Fused multiply-add / add-sub / sub-add.
    Fma,
    /// Width-changing float/int converts.
    Convert,
}

impl Family {
    /// Lower-case selector name used by `--family` and `--list`.
    pub fn name(self) -> &'static str {
        match self {
            Family::PackedInt => "packed_int",
            Family::HorizontalInt => "horizontal_int",
            Family::FloatHorizontal => "float_horizontal",
            Family::Permute => "permute",
            Family::Blend => "blend",
            Family::Shuffle => "shuffle",
            Family::Dup => "dup",
            Family::Fma => "fma",
            Family::Convert => "convert",
        }
    }
    /// All families, in table order — used by `--list` and to validate `--family`.
    pub const ALL: [Family; 9] = [
        Family::PackedInt,
        Family::HorizontalInt,
        Family::FloatHorizontal,
        Family::Permute,
        Family::Blend,
        Family::Shuffle,
        Family::Dup,
        Family::Fma,
        Family::Convert,
    ];
    /// Parse a `--family` selector (case-insensitive) to a `Family`.
    pub fn parse(s: &str) -> Option<Family> {
        let s = s.trim().to_ascii_lowercase();
        Family::ALL.into_iter().find(|f| f.name() == s)
    }
}

impl std::fmt::Display for Family {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One VEX/AVX2 fuzz op: a stable name, its [`Family`], and an emitter. Replaces the old
/// positional `match op { 0 => …, .. }` + magic `V_VEX_OPS` const (task-267). The *index*
/// into [`V_VEX`] is the op id carried in [`FuzzInsn::VVex`]; it is a direct index — no
/// modulo — so there is no `op%7`-vs-`op%63` drift. `emit(asm, d, a, b, imm)` assembles the
/// op with vector reg indices `d`/`a`/`b` (0..8) and an 8-bit `imm` control.
pub struct VexOp {
    pub name: &'static str,
    pub family: Family,
    pub emit: fn(&mut CodeAssembler, d: u8, a: u8, b: u8, imm: u8),
}

impl VexOp {
    /// True if this op interprets its operand bits as floating-point, so the fuzzer should
    /// bias register init toward FP corner values (task-268). The float-math families
    /// (float-horizontal, fma, width-changing float convert) plus the individually-float ops
    /// that live in mixed families: dpps + round (blend/shuffle), the permilps/pd lane
    /// permutes, the ps/pd immediate blends, and the float lane-dup moves. The remaining
    /// blend/shuffle/permute/dup entries are integer/byte movers and keep the integer pool.
    pub fn is_float(&self) -> bool {
        matches!(
            self.family,
            Family::FloatHorizontal | Family::Fma | Family::Convert
        ) || matches!(
            self.name,
            "vdpps"
                | "vroundps"
                | "vroundpd"
                | "vblendps"
                | "vblendpd"
                | "vblendvps"
                | "vblendvpd"
                | "vpermilps"
                | "vpermilpd"
                | "vmovddup"
                | "vmovshdup"
                | "vmovsldup"
        )
    }

    /// The float element widths (bytes) this op reads/produces, for NaN-payload-tolerant
    /// comparison (task-271). Empty for integer ops. `ph` converts touch both f16 and f32.
    /// The width must be constrained to what the op actually uses so a NaN encoding at one
    /// width can't alias another type's bit pattern (an f32 ±inf sign-flip vs an f16 NaN).
    pub fn fp_widths(&self) -> &'static [u32] {
        if !self.is_float() {
            &[]
        } else if self.name.contains("ph") {
            &[2, 4]
        } else if self.name.contains("pd") {
            &[8]
        } else if self.name.contains("ps") {
            &[4]
        } else {
            &[4, 8] // float lane-dup movers (vmovddup/sh/sl) — no ps/pd in the name
        }
    }
}

/// The set of float element widths (bytes) a program's float ops touch — the union over its
/// VEX float ops plus the legacy VNew round/horizontal-float/addsub entries (indices 0..=9).
/// Passed to [`crate::compare::compare_nan_tolerant`] so NaN tolerance is scoped to the widths
/// actually in play. Empty → the program has no float op → the compare stays strict.
fn prog_fp_widths(prog: &Prog) -> Vec<u32> {
    let mut w = std::collections::BTreeSet::new();
    for insn in &prog.insns {
        match insn {
            FuzzInsn::VVex { op, .. } => {
                for &b in V_VEX[*op as usize % V_VEX.len()].fp_widths() {
                    w.insert(b);
                }
            }
            // VNew 0..=9: roundps/pd/ss/sd, hadd/hsub ps/pd, addsub ps/pd — even = f32, odd = f64.
            FuzzInsn::VNew { op, .. } if (*op % V_NEW_OPS as u8) <= 9 => {
                w.insert(if (*op % V_NEW_OPS as u8) % 2 == 0 {
                    4
                } else {
                    8
                });
            }
            _ => {}
        }
    }
    w.into_iter().collect()
}

/// A 4th (mask) reg for the variable blends: derived from `b` exactly as the old code did.
fn blend_mask(b: u8) -> AsmRegisterYmm {
    ymm((b + 1) & 7)
}

/// The VEX/AVX2 op pool (task-259..264 sweep). Order — and therefore each op's index — is
/// UNCHANGED from the old positional table, so `gen_avx` draws a byte-identical RNG stream.
///
/// The old `_` catch-all did `match op % 7`, but with `op` already `op % 63` it could only
/// ever reach `vcvtps2ph` — the other six converts (vcvtdq2pd/ps2pd/pd2ps/pd2dq/tpd2dq/ph2ps)
/// were dead code. They are dropped here (keeping the pool at 63 and the emitted set exactly
/// what it was); re-adding them as real entries would grow the pool and shift the seeds, so
/// that belongs to its own task.
/// A plain 3-operand ymm VEX op (`asm.OP(ymm(d), ymm(a), ymm(b))`) — the common shape.
macro_rules! r3 {
    ($name:literal, $fam:expr, $op:ident) => {
        VexOp {
            name: $name,
            family: $fam,
            emit: |asm, d, a, b, _imm| {
                asm.$op(ymm(d), ymm(a), ymm(b)).unwrap();
            },
        }
    };
}

pub static V_VEX: &[VexOp] = &[
    // --- packed-int sat/avg/min-max/mulhrsw/pmadd (task-260), ymm 3-operand ---
    r3!("vpaddsb", Family::PackedInt, vpaddsb),
    r3!("vpaddsw", Family::PackedInt, vpaddsw),
    r3!("vpaddusb", Family::PackedInt, vpaddusb),
    r3!("vpaddusw", Family::PackedInt, vpaddusw),
    r3!("vpsubsb", Family::PackedInt, vpsubsb),
    r3!("vpsubsw", Family::PackedInt, vpsubsw),
    r3!("vpsubusb", Family::PackedInt, vpsubusb),
    r3!("vpsubusw", Family::PackedInt, vpsubusw),
    r3!("vpavgb", Family::PackedInt, vpavgb),
    r3!("vpavgw", Family::PackedInt, vpavgw),
    r3!("vpmaxsb", Family::PackedInt, vpmaxsb),
    r3!("vpmaxsw", Family::PackedInt, vpmaxsw),
    r3!("vpmaxuw", Family::PackedInt, vpmaxuw),
    r3!("vpminsb", Family::PackedInt, vpminsb),
    r3!("vpminsw", Family::PackedInt, vpminsw),
    r3!("vpminuw", Family::PackedInt, vpminuw),
    r3!("vpmulhrsw", Family::PackedInt, vpmulhrsw),
    r3!("vpmaddwd", Family::PackedInt, vpmaddwd),
    r3!("vpmaddubsw", Family::PackedInt, vpmaddubsw),
    // --- horizontal-int + sign, ymm (task-263) ---
    r3!("vphaddw", Family::HorizontalInt, vphaddw),
    r3!("vphaddd", Family::HorizontalInt, vphaddd),
    r3!("vphaddsw", Family::HorizontalInt, vphaddsw),
    r3!("vphsubw", Family::HorizontalInt, vphsubw),
    r3!("vphsubd", Family::HorizontalInt, vphsubd),
    r3!("vphsubsw", Family::HorizontalInt, vphsubsw),
    r3!("vpsadbw", Family::HorizontalInt, vpsadbw),
    r3!("vpsignb", Family::HorizontalInt, vpsignb),
    r3!("vpsignw", Family::HorizontalInt, vpsignw),
    r3!("vpsignd", Family::HorizontalInt, vpsignd),
    // --- float horizontal + addsub, ymm (task-261) ---
    r3!("vhaddps", Family::FloatHorizontal, vhaddps),
    r3!("vhaddpd", Family::FloatHorizontal, vhaddpd),
    r3!("vhsubps", Family::FloatHorizontal, vhsubps),
    r3!("vhsubpd", Family::FloatHorizontal, vhsubpd),
    r3!("vaddsubps", Family::FloatHorizontal, vaddsubps),
    r3!("vaddsubpd", Family::FloatHorizontal, vaddsubpd),
    // --- permutes (task-262) ---
    r3!("vpermilps", Family::Permute, vpermilps), // variable control
    r3!("vpermilpd", Family::Permute, vpermilpd),
    r3!("vpermps", Family::Permute, vpermps), // cross-lane gather
    // --- variable + imm blends (task-256/262) ---
    VexOp {
        name: "vpblendvb",
        family: Family::Blend,
        emit: |asm, d, a, b, _imm| {
            asm.vpblendvb(ymm(d), ymm(a), ymm(b), blend_mask(b))
                .unwrap();
        },
    },
    VexOp {
        name: "vblendvps",
        family: Family::Blend,
        emit: |asm, d, a, b, _imm| {
            asm.vblendvps(ymm(d), ymm(a), ymm(b), blend_mask(b))
                .unwrap();
        },
    },
    VexOp {
        name: "vblendvpd",
        family: Family::Blend,
        emit: |asm, d, a, b, _imm| {
            asm.vblendvpd(ymm(d), ymm(a), ymm(b), blend_mask(b))
                .unwrap();
        },
    },
    VexOp {
        name: "vblendps",
        family: Family::Blend,
        emit: |asm, d, a, b, imm| {
            asm.vblendps(ymm(d), ymm(a), ymm(b), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vblendpd",
        family: Family::Blend,
        emit: |asm, d, a, b, imm| {
            asm.vblendpd(ymm(d), ymm(a), ymm(b), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vpblendw",
        family: Family::Blend,
        emit: |asm, d, a, b, imm| {
            asm.vpblendw(ymm(d), ymm(a), ymm(b), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vmpsadbw",
        family: Family::Blend,
        emit: |asm, d, a, b, imm| {
            asm.vmpsadbw(ymm(d), ymm(a), ymm(b), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vdpps",
        family: Family::Blend,
        emit: |asm, d, a, b, imm| {
            asm.vdpps(ymm(d), ymm(a), ymm(b), imm as i32).unwrap();
        },
    },
    // --- imm 2-operand shuffles / byte-shifts / round / permil-imm (task-262/263) ---
    VexOp {
        name: "vpshufhw",
        family: Family::Shuffle,
        emit: |asm, d, a, _b, imm| {
            asm.vpshufhw(ymm(d), ymm(a), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vpshuflw",
        family: Family::Shuffle,
        emit: |asm, d, a, _b, imm| {
            asm.vpshuflw(ymm(d), ymm(a), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vpslldq",
        family: Family::Shuffle,
        emit: |asm, d, a, _b, imm| {
            asm.vpslldq(ymm(d), ymm(a), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vpsrldq",
        family: Family::Shuffle,
        emit: |asm, d, a, _b, imm| {
            asm.vpsrldq(ymm(d), ymm(a), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vroundps",
        family: Family::Shuffle,
        emit: |asm, d, a, _b, imm| {
            asm.vroundps(ymm(d), ymm(a), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vroundpd",
        family: Family::Shuffle,
        emit: |asm, d, a, _b, imm| {
            asm.vroundpd(ymm(d), ymm(a), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vpermilps", // imm control (distinct encoding from the variable form above)
        family: Family::Shuffle,
        emit: |asm, d, a, _b, imm| {
            asm.vpermilps(ymm(d), ymm(a), imm as i32).unwrap();
        },
    },
    VexOp {
        name: "vpermilpd", // imm control
        family: Family::Shuffle,
        emit: |asm, d, a, _b, imm| {
            asm.vpermilpd(ymm(d), ymm(a), imm as i32).unwrap();
        },
    },
    // --- lane-dup moves, ymm ---
    VexOp {
        name: "vmovddup",
        family: Family::Dup,
        emit: |asm, d, a, _b, _imm| {
            asm.vmovddup(ymm(d), ymm(a)).unwrap();
        },
    },
    VexOp {
        name: "vmovshdup",
        family: Family::Dup,
        emit: |asm, d, a, _b, _imm| {
            asm.vmovshdup(ymm(d), ymm(a)).unwrap();
        },
    },
    VexOp {
        name: "vmovsldup",
        family: Family::Dup,
        emit: |asm, d, a, _b, _imm| {
            asm.vmovsldup(ymm(d), ymm(a)).unwrap();
        },
    },
    // --- FMA add-sub / sub-add + a plain FMA control (task-261) ---
    r3!("vfmaddsub213ps", Family::Fma, vfmaddsub213ps),
    r3!("vfmaddsub213pd", Family::Fma, vfmaddsub213pd),
    r3!("vfmsubadd213ps", Family::Fma, vfmsubadd213ps),
    r3!("vfmsubadd213pd", Family::Fma, vfmsubadd213pd),
    r3!("vfmadd213ps", Family::Fma, vfmadd213ps),
    // --- width-changing convert (task-263): f32x8 -> f16x8, imm rounding ---
    VexOp {
        name: "vcvtps2ph",
        family: Family::Convert,
        emit: |asm, d, a, _b, imm| {
            asm.vcvtps2ph(xmm(d), ymm(a), (imm as i32) & 0x0f).unwrap();
        },
    },
];

/// Assemble one VEX/AVX2 op by direct index into [`V_VEX`] (no modulo). `op` is the id
/// carried in [`FuzzInsn::VVex`], guaranteed in range by generation.
fn vvex(asm: &mut CodeAssembler, op: u8, d: u8, aa: u8, bb: u8, imm: u8) {
    (V_VEX[op as usize].emit)(asm, d, aa, bb, imm);
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

// ======================= AVX/VEX differential fuzz campaign (task-267) =======================
//
// The two-leg (JIT-vs-interp + native-vs-interp) + shrink + dedup + coverage loop, lifted out
// of the old `tests/fuzz_avx.rs` driver so it lives in the library, is exercised by a fast
// `#[test]`, and backs the `cargo xfuzz` CLI (src/bin/fuzz.rs). The library now links the JIT
// backend directly (x86jit-cranelift moved to [dependencies]).

fn campaign_interp(p: &Prog) -> RunOutcome {
    run_with_backend_mode(
        &p.input(),
        Box::new(InterpreterBackend),
        GuestCpuFeatures::default(),
        p.mode,
    )
}
fn campaign_jit(p: &Prog) -> RunOutcome {
    run_with_backend_mode(
        &p.input(),
        Box::new(JitBackend::new()),
        GuestCpuFeatures::default(),
        p.mode,
    )
}
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn campaign_native(p: &Prog) -> Option<RunOutcome> {
    crate::native::run_native(&p.input())
}
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
fn campaign_native(_p: &Prog) -> Option<RunOutcome> {
    None
}

/// Whether the native (real host CPU) oracle is available — x86-64/Linux only.
pub fn native_available() -> bool {
    cfg!(all(target_arch = "x86_64", target_os = "linux"))
}

/// True if `prog` contains any VEX/AVX2 op — the campaign focuses its budget on these.
pub fn has_vex(p: &Prog) -> bool {
    p.insns.iter().any(|i| matches!(i, FuzzInsn::VVex { .. }))
}

/// Distinct VEX-op indices present in `prog`, sorted. Indices are direct into [`V_VEX`].
fn vex_ops_present(p: &Prog) -> Vec<usize> {
    let mut v: Vec<usize> = p
        .insns
        .iter()
        .filter_map(|i| match i {
            FuzzInsn::VVex { op, .. } => Some(*op as usize),
            _ => None,
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Dedup signature: the sorted distinct VEX-op *indices* (stable — no modulo). Keys the
/// per-(leg, signature) dedup so one campaign surfaces each distinct op-set bug once.
fn vex_sig(p: &Prog) -> String {
    vex_ops_present(p)
        .iter()
        .map(|o| o.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Comma-joined op NAMES of the distinct VEX ops in `prog` — the human-facing culprit list
/// (and the `--ops` argument a reader would use to focus a repro).
fn vex_names(p: &Prog) -> String {
    vex_ops_present(p)
        .iter()
        .map(|&o| V_VEX[o].name)
        .collect::<Vec<_>>()
        .join(",")
}

/// The full op pool (every [`V_VEX`] index) — the default campaign scope.
pub fn all_ops() -> Vec<usize> {
    (0..V_VEX.len()).collect()
}

/// Resolve a comma list of op names to their [`V_VEX`] indices (all forms sharing a name,
/// e.g. both `vpermilps` encodings). `Err(name)` on the first unrecognised name.
pub fn resolve_ops(names: &str) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for want in names.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let matched: Vec<usize> = V_VEX
            .iter()
            .enumerate()
            .filter(|(_, o)| o.name == want)
            .map(|(i, _)| i)
            .collect();
        if matched.is_empty() {
            return Err(want.to_string());
        }
        out.extend(matched);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Resolve a comma list of family selectors to the [`V_VEX`] indices they cover.
/// `Err(name)` on the first unrecognised family.
pub fn resolve_families(fams: &str) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for want in fams.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let fam = Family::parse(want).ok_or_else(|| want.to_string())?;
        for (i, o) in V_VEX.iter().enumerate() {
            if o.family == fam {
                out.push(i);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// A minimized diverging program plus its copy-paste reproducer.
#[derive(Clone)]
pub struct Finding {
    /// `"JIT-vs-interp"` or `"native-vs-interp"`.
    pub leg: &'static str,
    /// Seed that generates the (original) diverging program.
    pub seed: u64,
    /// Culprit op names (of the minimized program).
    pub ops: String,
    /// Copy-paste line, e.g. `cargo xfuzz --ops vcvtps2ph --seed 1964`.
    pub repro: String,
    /// Pretty-printed minimized instruction list.
    pub insns: String,
    /// The divergence detail.
    pub diff: String,
}

/// Per-op coverage counters, printed at the end of a run.
pub struct OpCov {
    pub idx: usize,
    pub name: &'static str,
    pub family: Family,
    /// Programs generated containing this op.
    pub generated: u64,
    /// Programs run through the native leg containing this op.
    pub native_run: u64,
    /// Distinct divergences whose minimized program contained this op.
    pub diverged: u64,
}

/// A finished campaign's totals, findings, and per-op coverage.
pub struct Report {
    pub checked: u64,
    pub native_run: u64,
    pub jit_hits: u64,
    pub native_hits: u64,
    pub distinct_bugs: usize,
    pub last_seed: u64,
    pub native_ok: bool,
    pub findings: Vec<Finding>,
    pub cov: Vec<OpCov>,
}

/// Campaign configuration. `vex_ops` subsets the [`V_VEX`] pool *before* generation.
pub struct CampaignCfg {
    /// Time budget (ignored when `single` is set).
    pub secs: u64,
    /// Program length (instructions).
    pub len: usize,
    /// First seed.
    pub start_seed: u64,
    /// If set, run exactly this one seed (deterministic replay) and return.
    pub single: Option<u64>,
    /// Allowed [`V_VEX`] indices (default: [`all_ops`]).
    pub vex_ops: Vec<usize>,
    /// Optional findings log file.
    pub log_path: Option<PathBuf>,
    /// Command prefix for `repro:` lines — the pool-selecting CLI args of this run so the
    /// reproducer regenerates the same program (e.g. `"cargo xfuzz --ops vcvtps2ph"`).
    pub repro_prefix: String,
    /// Print periodic status lines (each carrying the native-coverage fraction).
    pub status: bool,
    /// Suppress live per-finding output (the fast `#[test]` uses this).
    pub quiet: bool,
}

impl Default for CampaignCfg {
    fn default() -> Self {
        CampaignCfg {
            secs: 60,
            len: 12,
            start_seed: 1,
            single: None,
            vex_ops: all_ops(),
            log_path: None,
            repro_prefix: "cargo xfuzz".into(),
            status: true,
            quiet: false,
        }
    }
}

/// Run the AVX/VEX differential fuzz campaign (testing.md §7): generate VEX-bearing programs
/// from the (optionally subset) pool, check JIT-vs-interp and native-vs-interp, shrink and
/// dedup divergences, and tally per-op coverage. Does NOT stop on the first bug.
pub fn run_campaign(cfg: &CampaignCfg) -> Report {
    assert!(!cfg.vex_ops.is_empty(), "campaign vex_ops pool is empty");
    let native_ok = native_available();

    let mut cov: Vec<OpCov> = cfg
        .vex_ops
        .iter()
        .map(|&idx| OpCov {
            idx,
            name: V_VEX[idx].name,
            family: V_VEX[idx].family,
            generated: 0,
            native_run: 0,
            diverged: 0,
        })
        .collect();
    let cov_idx = |idx: usize| cfg.vex_ops.iter().position(|&x| x == idx);

    let mut log = cfg
        .log_path
        .as_ref()
        .and_then(|p| std::fs::File::create(p).ok());
    let mut seen: HashSet<String> = HashSet::new();
    let mut findings: Vec<Finding> = Vec::new();
    let (mut checked, mut native_run, mut jit_hits, mut native_hits) = (0u64, 0u64, 0u64, 0u64);
    let mut last_seed = cfg.start_seed;

    let deadline = Instant::now() + Duration::from_secs(cfg.secs);
    let period = Duration::from_secs((cfg.secs / 12).clamp(1, 30));
    let mut last_status = Instant::now();

    macro_rules! record {
        ($leg:expr, $min:expr, $diff:expr) => {{
            let min: &Prog = &$min;
            let sig = format!("{}:{}", $leg, vex_sig(min));
            if seen.insert(sig) {
                let ops = vex_names(min);
                let repro = format!("{} --seed {}", cfg.repro_prefix, min.seed);
                let finding = Finding {
                    leg: $leg,
                    seed: min.seed,
                    ops: ops.clone(),
                    repro: repro.clone(),
                    insns: format!("{:#?}", min.insns),
                    diff: $diff,
                };
                if !cfg.quiet {
                    let msg = format!(
                        "=== {} divergence (seed {}) ===\nops: {}\nrepro: {}\n{:#?}\n{}\n\n",
                        $leg, min.seed, ops, repro, min.insns, finding.diff
                    );
                    print!("{msg}");
                    if let Some(f) = log.as_mut() {
                        let _ = f.write_all(msg.as_bytes());
                        let _ = f.flush();
                    }
                }
                for op in vex_ops_present(min) {
                    if let Some(p) = cov_idx(op) {
                        cov[p].diverged += 1;
                    }
                }
                findings.push(finding);
            }
        }};
    }

    let mut seed = cfg.start_seed;
    loop {
        if cfg.single.is_none() && Instant::now() >= deadline {
            break;
        }
        let use_seed = cfg.single.unwrap_or(seed);
        let prog = gen_mode_ops(use_seed, cfg.len, CpuMode::Long64, true, Some(&cfg.vex_ops));
        last_seed = use_seed;
        if cfg.single.is_none() {
            seed += 1;
        }

        if !has_vex(&prog) {
            if cfg.single.is_some() {
                if !cfg.quiet {
                    println!("seed {use_seed}: no VEX op generated (nothing to check)");
                }
                break;
            }
            continue; // focus the budget on the VEX ops
        }
        checked += 1;
        for op in vex_ops_present(&prog) {
            if let Some(p) = cov_idx(op) {
                cov[p].generated += 1;
            }
        }

        let i = campaign_interp(&prog);
        let j = campaign_jit(&prog);
        if let Some(d) = compare(&i, &j, &[], &prog_fp_widths(&prog)) {
            let mut div = |p: &Prog| {
                compare(
                    &campaign_interp(p),
                    &campaign_jit(p),
                    &[],
                    &prog_fp_widths(p),
                )
                .is_some()
            };
            let min = shrink(&prog, &mut div);
            let dd = compare(
                &campaign_interp(&min),
                &campaign_jit(&min),
                &[],
                &prog_fp_widths(&min),
            )
            .unwrap_or(d);
            jit_hits += 1;
            record!("JIT-vs-interp", min, format!("{dd}"));
        }

        // Legacy-SSE vector ops PRESERVE bits 255:128 (audit task-266: interp matches the real
        // host CPU on all 62 probed; the only two that zeroed — packsswb/packssdw — were fixed in
        // task-269). So a program containing a legacy-SSE op with a dirty ymm upper is NOT native
        // noise; the native leg runs on every checked program.
        let native_this = native_ok;
        if native_this {
            if let Some(nat) = campaign_native(&prog) {
                native_run += 1;
                for op in vex_ops_present(&prog) {
                    if let Some(p) = cov_idx(op) {
                        cov[p].native_run += 1;
                    }
                }
                if let Some(d) = compare(&nat, &i, &dontcare_flags(&prog), &prog_fp_widths(&prog)) {
                    let mut div = |p: &Prog| {
                        campaign_native(p)
                            .map(|n| {
                                compare(
                                    &n,
                                    &campaign_interp(p),
                                    &dontcare_flags(p),
                                    &prog_fp_widths(p),
                                )
                                .is_some()
                            })
                            .unwrap_or(false)
                    };
                    let min = shrink(&prog, &mut div);
                    let dd = campaign_native(&min)
                        .and_then(|n| {
                            compare(
                                &n,
                                &campaign_interp(&min),
                                &dontcare_flags(&min),
                                &prog_fp_widths(&min),
                            )
                        })
                        .unwrap_or(d);
                    native_hits += 1;
                    record!("native-vs-interp", min, format!("{dd}"));
                }
            }
        }

        if cfg.single.is_some() {
            if !cfg.quiet && findings.is_empty() {
                let legs = if native_this {
                    "JIT-vs-interp + native-vs-interp"
                } else {
                    "JIT-vs-interp"
                };
                println!("seed {use_seed}: no divergence (checked {legs}).");
            }
            break;
        }

        if cfg.status && last_status.elapsed() >= period {
            let native_cov = if checked > 0 {
                native_run as f64 / checked as f64 * 100.0
            } else {
                0.0
            };
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            eprintln!(
                "[{left}s left] checked={checked} native_run={native_run} native_cov={native_cov:.1}% distinct_bugs={} (jit={jit_hits} native={native_hits}) seed={seed}",
                seen.len()
            );
            last_status = Instant::now();
        }
    }

    Report {
        checked,
        native_run,
        jit_hits,
        native_hits,
        distinct_bugs: seen.len(),
        last_seed,
        native_ok,
        findings,
        cov,
    }
}

/// Native-leg coverage fraction (percent): programs that reached the native oracle over all
/// checked programs. With the legacy-SSE skip gone (task-266), the native leg runs on every
/// checked program where the oracle is available, so this sits near 100% on x86-64/Linux;
/// surfacing it (in the status line and summary) keeps a "0 bugs" result auditable.
pub fn native_cov_pct(report: &Report) -> f64 {
    if report.checked > 0 {
        report.native_run as f64 / report.checked as f64 * 100.0
    } else {
        0.0
    }
}

/// Print the per-op coverage table (grouped by family) and the run summary with repro lines.
pub fn print_report(report: &Report) {
    println!("\n=== per-op coverage (generated / native_run / diverged) ===");
    for fam in Family::ALL {
        let rows: Vec<&OpCov> = report.cov.iter().filter(|c| c.family == fam).collect();
        if rows.is_empty() {
            continue;
        }
        println!("  [{}]", fam.name());
        for c in rows {
            println!(
                "    {:<16} gen={:<9} native={:<9} diverged={}",
                c.name, c.generated, c.native_run, c.diverged
            );
        }
    }
    if !report.native_ok {
        println!("  (native leg unavailable on this host — native columns stay 0)");
    }

    println!("\n=== summary ===");
    println!(
        "checked(with-vex)={} native_run={} native_cov={:.1}% distinct_bugs={} jit_hits={} native_hits={} last_seed={}",
        report.checked,
        report.native_run,
        native_cov_pct(report),
        report.distinct_bugs,
        report.jit_hits,
        report.native_hits,
        report.last_seed,
    );
    if report.findings.is_empty() {
        println!("no divergences.");
    } else {
        println!("findings ({}):", report.findings.len());
        for f in &report.findings {
            println!("  [{}] ops={}", f.leg, f.ops);
            println!("    repro: {}", f.repro);
        }
    }
}

#[cfg(test)]
mod campaign_tests {
    use super::*;

    #[test]
    fn table_is_63_ops_and_names_resolve() {
        assert_eq!(
            V_VEX.len(),
            63,
            "pool size must stay 63 (RNG-stream stability)"
        );
        // Every op resolves by name; families cover the whole pool.
        assert_eq!(resolve_ops("vcvtps2ph").unwrap(), vec![62]);
        assert_eq!(resolve_ops("vpaddsb,vpaddsw").unwrap(), vec![0, 1]);
        assert!(resolve_ops("nope").is_err());
        assert!(!resolve_families("convert,fma").unwrap().is_empty());
        assert!(resolve_families("bogus").is_err());
        // Both vpermilps encodings share the name and are both selected.
        assert_eq!(resolve_ops("vpermilps").unwrap(), vec![35, 52]);
    }

    #[test]
    fn full_pool_generation_is_byte_identical_to_gen_avx() {
        // gen_mode_ops with the full in-order pool must match gen_avx exactly (byte-for-byte
        // RNG stream) — the property that keeps existing seeds meaningful.
        let full = all_ops();
        for seed in [1u64, 42, 1964, 999_999] {
            let a = gen_avx(seed, 12);
            let b = gen_mode_ops(seed, 12, CpuMode::Long64, true, Some(&full));
            assert_eq!(
                format!("{:?}", a.insns),
                format!("{:?}", b.insns),
                "seed {seed}"
            );
            assert_eq!(a.init.xmm, b.init.xmm);
            assert_eq!(a.init.ymm_hi, b.init.ymm_hi);
        }
    }

    /// The FP-corner operand pool (task-268) must be inert on the non-AVX generators: `gen`
    /// and `gen32` still draw `vec128` for every xmm, so their RNG streams stay byte-identical
    /// to before it existed — the property `native_matches_interp`/`unicorn_matches_interp`
    /// depend on. These goldens were captured pre-change (`git show HEAD:…`) and verified equal
    /// post-change; if the FP pool ever leaks into the non-AVX path they change and this fails.
    #[test]
    fn non_avx_init_unchanged_by_fp_pool() {
        // (seed, gen() init.xmm, gen32() init.xmm) — captured from the pre-task-268 tree.
        #[rustfmt::skip]
        let cases: &[(u64, [u128; 8], [u128; 8])] = &[
            (1, [
                0x00ff00ff00ff00ff00ff00ff00ff00ff, 0x1eb967d7929813bb29663e9ea0ec2561,
                0xffffffffffffffffffffffffffffffff, 0x80008000800080008000800080008000,
                0x9fbd96359554aa53dc3320bb97ca63be, 0x1bea994d2e7d779da64b31c22cc57f39,
                0x0102030405060708090a0b0c0d0e0f10, 0xaf60baae69576109f0dad8272e600eb1,
            ], [
                0xc09a1a817914ffbc88b894e1401ed25b, 0x7fff7fff7fff7fff7fff7fff7fff7fff,
                0x00ff00ff00ff00ff00ff00ff00ff00ff, 0x1eb967d7929813bb29663e9ea0ec2561,
                0xffffffffffffffffffffffffffffffff, 0x80008000800080008000800080008000,
                0x9fbd96359554aa53dc3320bb97ca63be, 0x1bea994d2e7d779da64b31c22cc57f39,
            ]),
            (42, [
                0xffffffffffffffffffffffffffffffff, 0x7fff7fff7fff7fff7fff7fff7fff7fff,
                0x42577fcef4571016f6fd4f0b3ac5ea86, 0x0b7dcbd429a0baaa533054eb566050be,
                0x7fff7fff7fff7fff7fff7fff7fff7fff, 0xe43bef8e23a8e8bdeca4fb90109cfd66,
                0xac434f160c2d685b29f427733ef160f2, 0xcc4304242b442e02d11a235cac10079d,
            ], [
                0xffffffffffffffffffffffffffffffff, 0x7fff7fff7fff7fff7fff7fff7fff7fff,
                0x42577fcef4571016f6fd4f0b3ac5ea86, 0x0b7dcbd429a0baaa533054eb566050be,
                0x7fff7fff7fff7fff7fff7fff7fff7fff, 0xe43bef8e23a8e8bdeca4fb90109cfd66,
                0xac434f160c2d685b29f427733ef160f2, 0xcc4304242b442e02d11a235cac10079d,
            ]),
            (1964, [
                0x46c91629409ab29c9e50c6e50837f333, 0x0102030405060708090a0b0c0d0e0f10,
                0xffffffffffffffffffffffffffffffff, 0x13c742fbd4355bc3e039adf19f9a234c,
                0x00000000000000000000000000000000, 0x72e40c43c934fe659d98daaea1eeadf0,
                0xb3ca347b3ccc9efd51d7648fc1a3498b, 0xaf36977f0817ce6b85a8719c78820e71,
            ], [
                0x0102030405060708090a0b0c0d0e0f10, 0xffffffffffffffffffffffffffffffff,
                0x46c91629409ab29c9e50c6e50837f333, 0x0102030405060708090a0b0c0d0e0f10,
                0xffffffffffffffffffffffffffffffff, 0x13c742fbd4355bc3e039adf19f9a234c,
                0x00000000000000000000000000000000, 0x72e40c43c934fe659d98daaea1eeadf0,
            ]),
            (999_999, [
                0xffffffffffffffffffffffffffffffff, 0xffffffffffffffffffffffffffffffff,
                0xe3b9526d82da11087bb369e319b84eb1, 0x0102030405060708090a0b0c0d0e0f10,
                0xc7d5006c54e72fa891a3cfc0f126eec8, 0xffffffffffffffffffffffffffffffff,
                0x01dc24ebc5c861d120652ab2e816314d, 0xffffffffffffffffffffffffffffffff,
            ], [
                0x00000000000000000000000000000000, 0x3b6cfbd4a96fd7d1e93f6f856ae9ac8c,
                0xef44fb734cffd14af9d2739293093feb, 0xe3b9526d82da11087bb369e319b84eb1,
                0x0102030405060708090a0b0c0d0e0f10, 0xc7d5006c54e72fa891a3cfc0f126eec8,
                0xffffffffffffffffffffffffffffffff, 0x01dc24ebc5c861d120652ab2e816314d,
            ]),
        ];
        for &(seed, gxmm, g32xmm) in cases {
            // Only the low 8 xmm are seeded (the fuzzer's vector reg pool); the rest stay zero.
            assert_eq!(gen(seed, 12).init.xmm[..8], gxmm, "gen({seed}) xmm drifted");
            assert_eq!(
                gen32(seed, 12).init.xmm[..8],
                g32xmm,
                "gen32({seed}) xmm drifted"
            );
        }
    }

    /// The f32 corner set the generator draws from (mirror of `Rng::fp_lane32`), for the
    /// liveness assertions below.
    const FP32_CORNERS: [u32; 19] = [
        0x0000_0000,
        0x8000_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0000,
        0x7f80_0001,
        0x0000_0001,
        0x007f_ffff,
        0x0080_0000,
        0x7f7f_ffff,
        0x3f80_0000,
        0xbf80_0000,
        0x477f_e000,
        0x477f_f000,
        0x3880_0000,
        0x3380_0000,
        0x3300_0000,
        0x3fc0_0000,
        0x3f80_0001,
    ];

    /// `vec128_fp` must densely emit FP corner values — the whole point of task-268. Draw a
    /// stream and confirm f32 corner lanes (inf/NaN/subnormal/f16-boundary/…) show up in bulk.
    #[test]
    fn vec128_fp_emits_corner_lanes() {
        let corners: std::collections::HashSet<u32> = FP32_CORNERS.into_iter().collect();
        let mut rng = Rng::new(0xC0FFEE);
        let (mut lanes, mut corner_hits) = (0u32, 0u32);
        for _ in 0..4000 {
            let v = rng.vec128_fp();
            for i in 0..4 {
                lanes += 1;
                if corners.contains(&((v >> (i * 32)) as u32)) {
                    corner_hits += 1;
                }
            }
        }
        // The f32-pack path is ~1 draw in 4 and ~3 of its 4 lanes are corners, so well over 5%
        // of all lanes land on an f32 corner; a value the integer `vec128` pool never emits.
        assert!(
            corner_hits > lanes / 20,
            "vec128_fp emitted only {corner_hits}/{lanes} f32-corner lanes — pool too sparse"
        );
    }

    /// The FP pool must be WIRED into generation: an AVX program built from a float-only op pool
    /// draws its register init from `vec128_fp`, so f32 corner lanes appear in the init that the
    /// integer `vec128` pool would (almost) never produce.
    #[test]
    fn fp_pool_is_live_on_float_avx_programs() {
        let corners: std::collections::HashSet<u32> = FP32_CORNERS.into_iter().collect();
        let float_ops = resolve_families("convert,fma,float_horizontal").unwrap();
        let mut corner_hits = 0u32;
        for seed in 0..200u64 {
            let p = gen_mode_ops(seed, 12, CpuMode::Long64, true, Some(&float_ops));
            if !p.insns.iter().any(|i| matches!(i, FuzzInsn::VVex { .. })) {
                continue; // no float VEX op generated → integer init, skip
            }
            for reg in p.init.xmm.iter().chain(p.init.ymm_hi.iter()) {
                for i in 0..4 {
                    if corners.contains(&((reg >> (i * 32)) as u32)) {
                        corner_hits += 1;
                    }
                }
            }
        }
        assert!(
            corner_hits > 50,
            "float AVX programs drew only {corner_hits} FP-corner lanes — FP pool not wired"
        );
    }
}
