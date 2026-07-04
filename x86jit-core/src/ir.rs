//! Intermediate representation: three-address code over temporaries (§6).

/// A block-local temporary value. Reset per block (§7.2).
pub type Temp = u32;

/// An IR operand: a prior temporary or a constant.
#[derive(Copy, Clone, Debug)]
pub enum Val {
    Temp(Temp),
    Imm(u64),
}

/// Per-op memory ordering (§6.2, §8.2.3). The lift emits `None` for ordinary
/// accesses — the blanket policy for those comes from the Vm's `MemConsistency`
/// tier, applied at codegen time. Explicit values are for EXPLICIT guest
/// synchronization (locked ops, `mfence` lowering), honored in every tier.
#[derive(Copy, Clone, Debug)]
pub enum MemOrder {
    None,
    Release,
    Acquire,
}

/// jcc conditions. These map to flag combinations, distinguishing signed
/// (l/g) from unsigned (b/a) comparisons (§6.2).
#[derive(Copy, Clone, Debug)]
pub enum Cond {
    Eq,
    Ne, // ZF
    Below,
    BelowEq, // CF, CF|ZF  (unsigned)
    Above,
    AboveEq, // !CF&!ZF    (unsigned)
    Less,
    LessEq, // SF!=OF     (signed)
    Greater,
    GreaterEq, // SF==OF     (signed)
    Sign,
    NoSign, // SF
    Overflow,
    NoOverflow, // OF
    Parity,
    NoParity, // PF
}

/// Which flags an operation updates (§6.2). x86 is NOT all-or-nothing:
/// `inc`/`dec` keep CF; logic ops force CF=OF=0; shifts update flags only when
/// the count != 0 (runtime-conditional). A plain `bool` cannot express any of it.
/// Bit layout: CF=1, PF=2, AF=4, ZF=8, SF=16, OF=32.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct FlagMask(pub u8);

impl FlagMask {
    pub const NONE: FlagMask = FlagMask(0);
    pub const ALL: FlagMask = FlagMask(0b11_1111);
    pub const ALL_BUT_CF: FlagMask = FlagMask(0b11_1110); // inc/dec
                                                          // Shifts touch CF/PF/ZF/SF/OF (AF is left undefined) — and only when the
                                                          // masked count != 0 (a runtime condition the backends apply). (§16)
    pub const SHIFT: FlagMask = FlagMask(0b11_1011);
    // mul/imul touch only CF and OF (the rest are undefined).
    pub const CF_OF: FlagMask = FlagMask(0b10_0001);
    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// IR operations. Extended as the lift grows (§6.2). Any control-flow op ends
/// the block.
#[derive(Clone, Debug)]
pub enum IrOp {
    // --- instruction boundary marker (emitted by the lift at the start of each
    // guest instruction) ---
    // Carries the guest address so a backend can set cpu.rip = this address when a
    // load/store TRAPS (Unmapped/MMIO) or an exception fires (§8, §16). Without it
    // the RIP-on-trap / retry convention has no address to point at — guest_len only
    // gives the block END (correct for syscall, wrong for a mid-block fault).
    // Interpreter: keep it in a `cur_addr` local. JIT: bake it as a const for the
    // trapping accesses that follow. Also marks instruction boundaries for SMC.
    InsnStart {
        guest_addr: u64,
    },

    // --- data movement ---
    // NOTE: ReadReg(Reg::Rip) is FORBIDDEN — cpu.rip is stale mid-block; anything
    // that reads RIP is known statically at lift time and lowered to Imm (§6.2).
    ReadReg {
        dst: Temp,
        reg: crate::state::Reg,
    },
    // `size` (bytes: 1/2/4/8) drives the x86 sub-register write rule centrally: a
    // 4-byte write zeroes the upper 32 bits, 1/2-byte writes preserve them (§7.1,
    // §16). rip/fs/gs writes always use size 8. Applied by CpuState::write_gpr.
    WriteReg {
        reg: crate::state::Reg,
        src: Val,
        size: u8,
    },

    // --- arithmetic / logic (size in bytes: 1,2,4,8) ---
    Add {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    Sub {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    // Adc/Sbb CONSUME CF as input (a + b + CF); needed for `adc`/`sbb` and every
    // 128-bit add chain glibc/compilers emit. Flags computed over the full sum.
    Adc {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    Sbb {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    And {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    Or {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    Xor {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    Shl {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    Shr {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    // Arithmetic (sign-propagating) shift right — also lowers `cqo`/`cdq`.
    Sar {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    // Rotates. Only CF/OF are affected, and only when the masked count != 0
    // (CF_OF mask); OF is defined for count 1. (§16)
    Rol {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    Ror {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    // Sign-extend `a`'s low `from` bytes to 64 bits (movsx/movsxd/cdqe).
    Sext {
        dst: Temp,
        a: Val,
        from: u8,
    },
    // Reverse the byte order of the low `size` bytes (bswap; size 4 or 8). No flags.
    Bswap {
        dst: Temp,
        a: Val,
        size: u8,
    },
    // Widening multiply: `lo`/`hi` get the low/high `size`-byte halves of the
    // `size`-width product. `signed` picks imul vs mul. CF=OF set iff the product
    // doesn't fit in the low half (SF/ZF/PF/AF undefined → CF_OF mask). (§16)
    Mul {
        lo: Temp,
        hi: Temp,
        a: Val,
        b: Val,
        size: u8,
        signed: bool,
        set_flags: FlagMask,
    },
    // Divide the `size`-width `hi:lo` dividend by `divisor`: `quot`/`rem` get the
    // quotient/remainder. Raises `#DE` (a guest exception, not a lift error) on a
    // zero divisor or a quotient that overflows the destination. Flags undefined.
    Div {
        quot: Temp,
        rem: Temp,
        hi: Val,
        lo: Val,
        divisor: Val,
        size: u8,
        signed: bool,
    },
    // ... Mul, Div, Rol, Ror, etc.

    // --- flags as DATA (setcc, cmovcc, adc/sbb lowering, rcl/rcr) ---
    // Materialize a condition as 0/1 (§6.2). CF alone = GetCond(Below).
    GetCond {
        dst: Temp,
        cond: Cond,
    },

    // --- memory ---
    Load {
        dst: Temp,
        addr: Val,
        size: u8,
    },
    Store {
        addr: Val,
        src: Val,
        size: u8,
        order: MemOrder,
    },

    // bt/bts/btr/btc: CF <- bit `bit % (size*8)` of `a`; `result` is `a` with that
    // bit set/cleared/toggled (unchanged for plain `bt`). Only CF is written (the
    // other flags are left as x86 leaves them undefined).
    Bt {
        result: Temp,
        a: Val,
        bit: Val,
        size: u8,
        op: BtOp,
    },

    // cpuid: leaf in EAX, subleaf in ECX -> EAX/EBX/ECX/EDX. Handled by a shared
    // routine so both backends answer identically (§14).
    Cpuid,

    // x87 FPU op (§14). `addr` is the effective address for memory forms (ignored
    // otherwise); `sti` selects ST(i) for register forms. Executed by the shared
    // `exec_x87` in both backends. May trap on a memory access.
    X87 {
        kind: crate::x87::FpuKind,
        addr: Val,
        sti: u8,
    },

    // popcnt: `dst` = set-bit count of `src`. ZF <- (src == 0); CF/OF/SF/AF/PF = 0.
    Popcnt {
        dst: Temp,
        src: Val,
        size: u8,
    },

    // crc32 (SSE4.2): accumulate the CRC-32C of `src`'s low `bytes` bytes into the
    // running CRC `crc` (the destination register). No flags. Shared routine.
    Crc32 {
        dst: Temp,
        crc: Val,
        src: Val,
        bytes: u8,
    },

    // bsf/bsr: index of the lowest (`reverse`=false) / highest (`reverse`=true) set
    // bit of `src`. If `src`==0: ZF=1 and `dst` keeps `old` (destination undefined
    // per Intel, but real CPUs preserve it); else ZF=0 and `dst` = the bit index.
    // Only ZF is defined.
    BitScan {
        dst: Temp,
        src: Val,
        old: Val,
        size: u8,
        reverse: bool,
    },

    // --- atomics (§8.2.3, §11). Fully ordered in every consistency tier. ---
    // Atomic read-modify-write: `[addr] = op([addr], src)`, `old` <- prior value.
    // Sets NO flags — the lift emits a separate ALU op on `old`/`src` (reusing the
    // materialized-flag machinery) so locked ALU ops flag exactly like their plain
    // forms. Backs `lock add`/`and`/`or`/`xor`/`sub`/`inc`/`dec`, `xadd`, `xchg`.
    AtomicRmw {
        old: Temp,
        addr: Val,
        src: Val,
        size: u8,
        op: RmwOp,
    },
    // Atomic compare-exchange (`cmpxchg`): if `[addr] == expected` then
    // `[addr] = src`; `old` <- prior value either way. ZF/etc. come from a
    // separate `cmp expected, old` the lift emits.
    AtomicCas {
        old: Temp,
        addr: Val,
        expected: Val,
        src: Val,
        size: u8,
    },

    // --- SIMD (SSE, §3.1 M8). XMM registers by index (0..15), 128-bit values. ---
    // Load 16/8/4 bytes from memory into xmm `dst` (upper bytes zeroed for <16).
    VLoad {
        dst: u8,
        addr: Val,
        size: u8,
    },
    VStore {
        addr: Val,
        src: u8,
        size: u8,
    },
    // xmm -> xmm move (movaps/movdqa/movdqu reg form).
    VMov {
        dst: u8,
        src: u8,
    },
    // movd/movq: GPR/imm -> xmm low, upper zeroed (`size` 4 or 8).
    VFromGpr {
        dst: u8,
        src: Val,
        size: u8,
    },
    // movd/movq: xmm low `size` bytes -> GPR temp.
    VToGpr {
        dst: Temp,
        src: u8,
        size: u8,
    },
    // Bitwise vector logic: pxor/pand/por/pandn (and the *ps aliases).
    VLogic {
        dst: u8,
        a: u8,
        b: u8,
        op: VLogicOp,
    },
    // Packed integer arithmetic per `lane` bytes (1/2/4/8): padd*/psub*/pcmpeq*.
    VPackedBin {
        dst: u8,
        a: u8,
        b: u8,
        lane: u8,
        op: PackedBinOp,
    },
    // Memory-source forms: `dst = op(dst, load128(addr))` (e.g. `paddd xmm,[mem]`).
    VPackedBinM {
        dst: u8,
        addr: Val,
        lane: u8,
        op: PackedBinOp,
    },
    VLogicM {
        dst: u8,
        addr: Val,
        op: VLogicOp,
    },
    // Packed shift of each `lane`-byte element by `imm`: left (`right`=false) or
    // right; a right shift is arithmetic (sign-propagating) when `arith` else
    // logical (psll*/psrl*/psra*).
    VPackedShift {
        dst: u8,
        a: u8,
        imm: u8,
        lane: u8,
        right: bool,
        arith: bool,
    },
    // Byte-shift the whole 128-bit value by `bytes`, right if `right` else left
    // (psrldq/pslldq); vacated bytes are zero.
    VByteShift {
        dst: u8,
        a: u8,
        bytes: u8,
        right: bool,
    },
    // pshufd: permute the four 32-bit lanes of `a` per the imm8 selector.
    VShuffle32 {
        dst: u8,
        a: u8,
        imm: u8,
    },
    // pshuflw (`high`=false) / pshufhw (`high`=true): permute the four 16-bit words
    // of the low (resp. high) 64 bits per imm8; the other half is copied unchanged.
    VShuffle16 {
        dst: u8,
        a: u8,
        imm: u8,
        high: bool,
    },
    // shufps: dst lanes 0,1 selected from `a`'s 32-bit lanes, lanes 2,3 from `b`'s,
    // per the imm8 (2 bits each).
    VShufps {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
    },
    // pshufb (SSSE3): `dst[i] = (idx[i] & 0x80) ? 0 : dst[i's low nibble of idx]`.
    // Index vector from a register or memory (`VPshufbM`).
    VPshufb {
        dst: u8,
        idx: u8,
    },
    VPshufbM {
        dst: u8,
        addr: Val,
    },
    // punpckl*/punpckh*: interleave the low (`high`=false) or high halves of `a`
    // and `b` at `lane`-byte granularity.
    VUnpackLow {
        dst: u8,
        a: u8,
        b: u8,
        lane: u8,
        high: bool,
    },
    // packuswb: pack 8+8 signed 16-bit lanes to 16 unsigned-saturated bytes.
    VPackUsWB {
        dst: u8,
        a: u8,
        b: u8,
    },
    // pinsrw: insert the low 16 bits of `src` into word lane `index` of `dst`.
    VInsertW {
        dst: u8,
        src: Val,
        index: u8,
    },
    // pextrw: extract word lane `index` of xmm `src` into gpr `dst` (zero-extended).
    VExtractW {
        dst: Temp,
        src: u8,
        index: u8,
    },
    // pextrb/pextrd/pextrq: extract the `size`-byte lane (`size` ∈ {1,4,8}) at `index`
    // of xmm `src` into `dst`, zero-extended.
    VExtractLane {
        dst: Temp,
        src: u8,
        index: u8,
        size: u8,
    },
    // pmovmskb: the high bit of each of the 16 bytes of `src` → low 16 bits of gpr `dst`.
    VMoveMaskB {
        dst: Temp,
        src: u8,
    },

    // --- SSE/SSE2 floating point (§3.1 M8). ---
    // Scalar/packed float arithmetic: add/sub/mul/div{ss,sd,ps,pd}. `scalar` =
    // operate on lane 0 only, preserving the upper bytes of `dst`; else every
    // `prec`-wide lane. `a` is `dst` for the two-operand x86 form.
    VFloatBin {
        dst: u8,
        a: u8,
        b: u8,
        op: FloatBinOp,
        prec: FPrec,
        scalar: bool,
    },
    // Memory-source form: `dst = op(dst, mem)`. Loads `prec` bytes when `scalar`,
    // else the full 16.
    VFloatBinM {
        dst: u8,
        addr: Val,
        op: FloatBinOp,
        prec: FPrec,
        scalar: bool,
    },
    // movss/movsd reg,reg: merge the low `prec`-wide lane of `src` into `dst`,
    // preserving `dst`'s upper bytes (distinct from the zero-extending mem form).
    VFloatMov {
        dst: u8,
        src: u8,
        prec: FPrec,
    },
    // ucomis{s,d}/comis{s,d}: set ZF/PF/CF from an ordered float compare of the low
    // lanes (`a`,`b` are the raw float bits), clearing OF/SF/AF. Unordered → all set.
    VFloatCmp {
        a: Val,
        b: Val,
        prec: FPrec,
    },
    // cmp{ss,sd,ps,pd}: compare each lane per the `pred` (imm8: EQ/LT/LE/UNORD/
    // NEQ/NLT/NLE/ORD) → all-ones or zero mask. `scalar` = lane 0 only, upper of
    // `dst` preserved. `a` is `dst`.
    VFloatCmpMask {
        dst: u8,
        a: u8,
        b: u8,
        prec: FPrec,
        scalar: bool,
        pred: u8,
    },
    // cvtsi2s{s,d}: signed `int_size`-byte integer `src` -> float in `dst`'s low
    // lane, preserving the upper bytes.
    VCvtFromInt {
        dst: u8,
        src: Val,
        int_size: u8,
        prec: FPrec,
    },
    // cvt(t)s{s,d}2si: `prec`-wide float `src` (raw bits) -> signed `int_size`-byte
    // integer in `dst`. `trunc` = toward zero (cvtt*), else round to nearest even.
    VCvtToInt {
        dst: Temp,
        src: Val,
        int_size: u8,
        prec: FPrec,
        trunc: bool,
    },
    // cvtss2sd/cvtsd2ss: convert the low-lane float `src` (raw bits) from `from` to
    // `to` precision into `dst`'s low lane, preserving the upper bytes.
    VCvtFloat {
        dst: u8,
        src: Val,
        from: FPrec,
        to: FPrec,
    },
    // sqrts{s,d}/sqrtp{s,d}: `scalar` = lane 0 only (upper preserved), else all
    // lanes. Register source.
    VFloatUnary {
        dst: u8,
        src: u8,
        op: FloatUnOp,
        prec: FPrec,
        scalar: bool,
    },
    // movlhps/movhlps: copy one 64-bit half of `src` into one half of `dst`, the
    // other half of `dst` preserved.
    VMoveHalf {
        dst: u8,
        src: u8,
        dst_high: bool,
        src_high: bool,
    },
    // movhps/movlps load: 8 bytes from memory into the high/low half of `dst`.
    VLoadHalf {
        dst: u8,
        addr: Val,
        high: bool,
    },
    // movhps/movlps store: the high/low 8 bytes of `src` to memory.
    VStoreHalf {
        addr: Val,
        src: u8,
        high: bool,
    },

    // --- string ops (§10). ---
    // Set/clear the direction flag (std/cld).
    SetDf {
        value: bool,
    },
    // A movs/stos/scas/cmps/lods, optionally `rep`/`repe`/`repne`. Runs the whole
    // (restartable) loop; updates RSI/RDI/RCX/flags. May trap on a memory access
    // (RIP left on the instruction for retry).
    RepString {
        op: StrOp,
        elem: u8,
        rep: RepKind,
    },

    // --- control flow: each of these ENDS the block ---
    Jump {
        target: Val,
    }, // direct: Imm, indirect: Temp
    Branch {
        cond: Cond,
        taken: u64,
        fallthrough: u64,
    }, // jcc — both targets known
    Call {
        target: Val,
        return_addr: u64,
    },
    Ret,
    Syscall,
    Hlt,
}

/// Bitwise vector logic op (§3.1 M8).
#[derive(Copy, Clone, Debug)]
pub enum VLogicOp {
    Xor,
    And,
    Or,
    Andn,
}

/// Packed integer arithmetic op (§3.1 M8).
#[derive(Copy, Clone, Debug)]
pub enum PackedBinOp {
    Add,
    Sub,
    CmpEq,
    /// `pcmpgt*` — signed greater-than (per lane, all-ones / zero).
    CmpGt,
    MinU,
    MaxU,
    MinS,
    MaxS,
}

/// Bit-test operation (`bt`/`bts`/`btr`/`btc`).
#[derive(Copy, Clone, Debug)]
pub enum BtOp {
    /// `bt` — test only.
    Test,
    /// `bts` — set the bit.
    Set,
    /// `btr` — clear the bit.
    Reset,
    /// `btc` — toggle the bit.
    Complement,
}

/// Atomic read-modify-write operation (§8.2.3). `Xchg` ignores the current value
/// (unconditional store); the rest combine it with the source.
#[derive(Copy, Clone, Debug)]
pub enum RmwOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Xchg,
}

/// Floating-point element width for scalar/packed SSE float ops (§3.1 M8).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FPrec {
    /// 32-bit `f32` (`*ss`/`*ps`).
    F32,
    /// 64-bit `f64` (`*sd`/`*pd`).
    F64,
}

impl FPrec {
    /// Byte width of one lane.
    pub fn bytes(self) -> u8 {
        match self {
            FPrec::F32 => 4,
            FPrec::F64 => 8,
        }
    }
}

/// Scalar/packed floating-point arithmetic op (§3.1 M8). `Min`/`Max` use x86 SSE
/// semantics: on a NaN operand or equal values, the second operand wins.
#[derive(Copy, Clone, Debug)]
pub enum FloatBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

/// Scalar/packed floating-point unary op (§3.1 M8).
#[derive(Copy, Clone, Debug)]
pub enum FloatUnOp {
    Sqrt,
}

/// String operation (§10).
#[derive(Copy, Clone, Debug)]
pub enum StrOp {
    Movs,
    Stos,
    Scas,
    Cmps,
    Lods,
}

/// Repeat prefix on a string op (§10).
#[derive(Copy, Clone, Debug)]
pub enum RepKind {
    None,
    Rep,
    Repe,
    Repne,
}

/// A lifted basic block, keyed by guest start address in the cache (§6.3).
#[derive(Clone, Debug)]
pub struct IrBlock {
    pub guest_start: u64,
    pub ops: Vec<IrOp>,
    /// Number of temporaries allocated (backend reserves this many slots).
    pub temp_count: u32,
    /// Guest bytes covered by the block (for SMC invalidation).
    pub guest_len: u32,
    /// x86 instruction count (for budget/stats).
    pub icount: u32,
}

/// Bounds on how large a superblock region may grow (§12 M5-T3). Region formation
/// stops when either is reached, keeping compile time and code size bounded.
#[derive(Copy, Clone, Debug)]
pub struct RegionCaps {
    pub max_blocks: usize,
    pub max_icount: u32,
}

/// A superblock: a sequence of basic blocks compiled into one function (§12 M5-T3).
/// `blocks[0]` starts at `entry`; the region's internal control flow connects the
/// rest. Sub-blocks may be non-contiguous, so SMC invalidation uses [`spans`].
#[derive(Clone, Debug)]
pub struct IrRegion {
    pub entry: u64,
    pub blocks: Vec<IrBlock>,
    /// Whether the region contains a back-edge (a loop). Only loop regions amortize
    /// their (heavier) compile over many iterations, so the dispatcher forms a
    /// region only when this holds; loop-free code stays single-block (§12 M5-T3f).
    pub has_loop: bool,
}

impl IrRegion {
    /// Guest byte ranges the region covers (one per sub-block) — for SMC.
    pub fn spans(&self) -> Vec<(u64, u32)> {
        self.blocks
            .iter()
            .map(|b| (b.guest_start, b.guest_len))
            .collect()
    }
}

/// Monotonic per-block temporary allocator (§7.2).
pub struct TempGen(u32);

impl TempGen {
    pub fn new() -> Self {
        TempGen(0)
    }
    pub fn fresh(&mut self) -> Temp {
        let t = self.0;
        self.0 += 1;
        t
    }
    pub fn count(&self) -> u32 {
        self.0
    }
}

impl Default for TempGen {
    fn default() -> Self {
        Self::new()
    }
}
