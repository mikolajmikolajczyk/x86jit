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
    // Double-precision shift (`SHLD`/`SHRD`): shift `a` by `count` (masked mod
    // width), filling the vacated bits from `b`. `left` picks SHLD (fill low from b's
    // high) vs SHRD (fill high from b's low). CF = last bit shifted out of `a`,
    // SF/ZF/PF from the result; OF defined only for count 1; a masked count of 0 is a
    // no-op leaving flags unchanged. (§16)
    DoubleShift {
        dst: Temp,
        a: Val,
        b: Val,
        count: Val,
        size: u8,
        left: bool,
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
    // Rotate-through-carry (rcl/rcr): rotate a (size*8 + 1)-bit value that includes CF.
    // Unlike Rol/Ror these CONSUME CF as input (like Adc/Sbb). Only CF/OF are affected,
    // count-conditional; OF defined for count 1. Go's div-by-constant strength reduction
    // emits `rcr r/m,1` to fold the multiply's carry back in. (§16, task-132)
    Rcl {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        set_flags: FlagMask,
    },
    Rcr {
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

    // xgetbv: EDX:EAX = extended control register selected by ECX. Guests read XCR0
    // (ECX=0). Runtime op (not baked at lift time) so XCR0 tracks the embedder's
    // feature set (task-169) — `cpu.features.xcr0()`.
    Xgetbv,

    // x87 FPU op (§14). `addr` is the effective address for memory forms (ignored
    // otherwise); `sti` selects ST(i) for register forms. Executed by the shared
    // `exec_x87` in both backends. May trap on a memory access.
    X87 {
        kind: crate::x87::FpuKind,
        addr: Val,
        sti: u8,
    },

    // fxsave/fxrstor (§14): save/restore the 512-byte legacy FP/SSE state at the
    // effective address. `restore` = fxrstor. XMM + FCW round-trip faithfully; MXCSR
    // is the default (not modeled), x87 via the f64-backed converters. Executed by
    // the shared `exec_fxstate` in both backends; may trap on the memory access.
    FxState {
        addr: Val,
        restore: bool,
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

    // bsf/bsr/tzcnt/lzcnt — one op, the variant is a `BitScanOp` (conventions.md:
    // family = enum). bsf/bsr set only ZF and keep `old` on a zero source; tzcnt/lzcnt
    // are defined on zero (= operand bit-width) and set ZF (result==0) + CF (src==0).
    BitScan {
        dst: Temp,
        src: Val,
        old: Val,
        size: u8,
        op: BitScanOp,
    },

    // BMI1/BMI2 single-dst bit op (task-168.5.3): `dst = op(a, b)` at `size` (4/8),
    // flags per `BmiOp`. The unary bls* ops ignore `b`.
    Bmi {
        dst: Temp,
        a: Val,
        b: Val,
        size: u8,
        op: BmiOp,
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
    // palignr (SSSE3): concatenate `dst` (high 16 bytes) with the source (low 16),
    // shift the 32-byte value right by `imm` bytes, keep the low 16. Source from a
    // register (`VAlignr`) or memory (`VAlignrM`).
    VAlignr {
        dst: u8,
        src: u8,
        imm: u8,
    },
    VAlignrM {
        dst: u8,
        addr: Val,
        imm: u8,
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
    // pinsrb/pinsrd/pinsrq (+ VEX vpinsr): xmm `dst` = xmm `base` with its `size`-byte
    // lane (`size` ∈ {1,4,8}) at `index` replaced by the low `size` bytes of `src`.
    // Legacy form passes `base == dst`; the VEX form passes src1 and zeroes the upper
    // bits via a following VZeroUpper (task-168.5 grind).
    VInsertLane {
        dst: u8,
        base: u8,
        src: Val,
        index: u8,
        size: u8,
    },
    // pmovmskb: the high bit of each of the 16 bytes of `src` → low 16 bits of gpr `dst`.
    VMoveMaskB {
        dst: Temp,
        src: u8,
    },

    // --- AVX upper-half state (task-168.2). ---
    /// Zero the upper 128 bits of YMM `reg` — a VEX.128 write clears bits 255:128.
    VZeroUpper {
        reg: u8,
    },
    /// `vzeroupper`: zero the upper 128 bits of every YMM register.
    VZeroUpperAll,

    // --- AVX-256 (VEX.256) data movement (task-168.2). A 256-bit vector = the low
    // 128 (`xmm[reg]`) plus the high 128 (`ymm_hi[reg]`). ---
    // Width-parameterized VEX/EVEX data movement (task-170.2): `bytes` (32 or 64) as
    // `bytes/16` 128-bit lanes over xmm/ymm_hi/zmm_hi, gathered/zero-extended by
    // `vec_lanes`/`set_vec`. The SSE 128-bit VLoad/VStore/VMov (preserve-upper
    // semantics) stay separate above.
    /// Load `bytes` (32/64) from `[addr]` into the low lanes of vector `dst`.
    VLoadWide {
        dst: u8,
        addr: Val,
        bytes: u16,
    },
    /// Store `bytes` (32/64) from vector `src` to `[addr]`.
    VStoreWide {
        addr: Val,
        src: u8,
        bytes: u16,
    },
    /// Copy the low `bytes` (32/64) of vector `src` into `dst`.
    VMovWide {
        dst: u8,
        src: u8,
        bytes: u16,
    },
    /// EVEX masked register move `vmovdqu{32,64}/vmovdqa{32,64} v{k}{z}, v` (task-170.1,
    /// decision-13): commit `src` into `dst` under opmask `k` at `elem`-byte granularity
    /// across `bytes` (16/32/64), merging or zeroing per `zeroing`. Delegates to the
    /// shared `CpuState::write_masked`.
    VMaskMov {
        dst: u8,
        src: u8,
        k: u8,
        elem: u8,
        zeroing: bool,
        bytes: u16,
    },
    /// EVEX write-masked vector **load** `vmovdqu{8,16,32,64} v{k}{z}, [mem]` (task-168.5.5):
    /// load the low `bytes` of `dst` element-wise from `addr` under opmask `k` at `elem`-byte
    /// granularity — masked-off lanes are zeroed (`zeroing`) or kept (merge) and never touch
    /// memory (hardware fault suppression). glibc's AVX-512 string routines head/tail with
    /// this. Delegates to the shared `masked_load_run` (JIT == interp, fault-capable).
    VMaskLoadMem {
        dst: u8,
        addr: Val,
        k: u8,
        elem: u8,
        zeroing: bool,
        bytes: u16,
    },
    /// EVEX write-masked vector **store** `vmovdqu{8,16,32,64} [mem]{k}, v` (task-168.5.5):
    /// store the active `k` lanes of `src` to `addr` element-wise (no zeroing form). Inactive
    /// lanes never touch memory. Delegates to the shared `masked_store_run`.
    VMaskStoreMem {
        src: u8,
        addr: Val,
        k: u8,
        elem: u8,
        bytes: u16,
    },
    /// 256-bit bitwise logic `dst = op(a, b)` applied to both 128-bit halves.
    VLogic256 {
        dst: u8,
        a: u8,
        b: u8,
        op: VLogicOp,
    },
    /// 256-bit logic with a 32-byte memory source: `dst = op(a, [addr..32])`.
    VLogic256M {
        dst: u8,
        a: u8,
        addr: Val,
        op: VLogicOp,
    },
    /// Width-generic EVEX bitwise logic `dst = op(a, b)` over `bytes` (16/32/64) —
    /// `vpxor{d,q}`/`vpand{d,q}`/`vpor{d,q}`/`vpandn{d,q}` (task-168.5.2). Bitwise, so
    /// the `d`/`q` element suffix is irrelevant unmasked; writes clear the register above
    /// `bytes` (VEX/EVEX upper-zeroing). Register src2 only; masked forms are deferred.
    VLogicWide {
        dst: u8,
        a: u8,
        b: u8,
        op: VLogicOp,
        bytes: u16,
    },
    /// Memory-source `src2` form of [`IrOp::VLogicWide`] (task-195): `b` is a `bytes`-wide
    /// vector at `addr` (`vpxorq xmm, xmm, [mem]`). glibc folds the second logic operand.
    VLogicWideM {
        dst: u8,
        a: u8,
        addr: Val,
        op: VLogicOp,
        bytes: u16,
    },
    /// AVX512-VPOPCNTDQ `vpopcnt{d,q}` (task-195): per `lane`-byte element over the low
    /// `bytes` (16/32/64), `dst[i] = popcount(a[i])`; clears the register above `bytes`.
    /// CachyOS builds coreutils with VPOPCNTDQ on, so `sort`/`base64` hit `vpopcntq zmm`.
    VPopcnt {
        dst: u8,
        a: u8,
        lane: u8,
        bytes: u16,
    },
    /// Memory-source form of [`IrOp::VPopcnt`] (task-195): `a` is a `bytes`-wide vector at
    /// `addr` (`vpopcntq zmm, [mem]`).
    VPopcntM {
        dst: u8,
        addr: Val,
        lane: u8,
        bytes: u16,
    },
    /// EVEX lane insert `vinserti32x4`/`vinserti64x2`/`vinserti64x4` (task-168.5.6):
    /// `dst = src` with the `idx`-th group of `num_lanes` 128-bit lanes replaced by the
    /// low lanes of `ins`. `num_lanes` is 1 for a 128-bit insert, 2 for a 256-bit insert.
    VInsertLaneWide {
        dst: u8,
        src: u8,
        ins: u8,
        idx: u8,
        num_lanes: u8,
        bytes: u16,
    },
    /// EVEX lane extract `vextracti32x4`/`vextracti64x2`/`vextracti32x8`/`vextracti64x4`
    /// (task-195): `dst` (128- or 256-bit) = the `idx`-th group of `num_lanes` 128-bit lanes
    /// of `src`, zero-extended above. `num_lanes` is 1 (128-bit extract) or 2 (256-bit).
    VExtractLaneWide {
        dst: u8,
        src: u8,
        idx: u8,
        num_lanes: u8,
    },
    /// SSE4.2 `pcmpistri`/`pcmpestri` (task-168.5.4): string-compare aggregation writing
    /// the index to ECX and CF/ZF/SF/OF. `b` is a register (memory deferred); `explicit`
    /// selects `pcmpestri` (lengths from EAX/EDX) vs `pcmpistri` (implicit null length).
    VPcmpStr {
        a: u8,
        b: u8,
        imm: u8,
        explicit: bool,
    },
    /// As [`VPcmpStr`] but source 2 is a memory operand `[addr]` — the loaded 128-bit
    /// value is compared against `cpu.xmm[a]` (task-195). glibc's SSE4.2 `strchr`/`strstr`
    /// use `pcmpistri xmm, [mem], imm`. A fault on the load traps like any vector load.
    VPcmpStrM {
        a: u8,
        addr: Val,
        imm: u8,
        explicit: bool,
    },
    /// EVEX `valignd`/`valignq` (task-168.5.6): shift the concatenation `a:b` (a high, b
    /// low) right by `shift` elements of `elem` bytes and keep the low `bytes`.
    VAlign {
        dst: u8,
        a: u8,
        b: u8,
        shift: u8,
        elem: u8,
        bytes: u16,
    },
    /// SSE4.1 `pmovzx`/`pmovsx` (task-168.5.4): read `16/to` low elements of `from`
    /// bytes each from `src` (a register's low bytes), zero- or sign-extend each to `to`
    /// bytes, and write the 128-bit result to `dst` (`from` < `to`, both powers of two).
    VPMovExtend {
        dst: u8,
        src: u8,
        from: u8,
        to: u8,
        signed: bool,
    },
    /// As [`IrOp::VPMovExtend`] but the `16/to * from` source bytes come from memory.
    VPMovExtendM {
        dst: u8,
        addr: Val,
        from: u8,
        to: u8,
        signed: bool,
    },
    /// EVEX/VEX-256 widening move `vpmov{s,z}x{bw,bd,bq,wd,wq,dq}` to a ymm/zmm dest, or
    /// the masked xmm form (task-195): zero/sign-extend `dst_width/to` low `from`-byte
    /// source lanes to `to` bytes each; bits above the packed result are zeroed (EVEX
    /// dest). Masked/zeroing per `writemask` at `to` granularity. Register src only.
    /// Cold/masked → shared `exec_vpmov_extend_wide` (jit == interp).
    VPMovExtendWide {
        dst: u8,
        src: u8,
        from: u8,
        to: u8,
        signed: bool,
        dst_width: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// Packed absolute value `vpabs{b,w,d,q}` (VEX/EVEX, task-195): per `elem`-byte lane,
    /// `dst = |src|` (signed; `abs(MIN)` wraps to `MIN`, matching x86). Any width; bits
    /// above `dst_width` zeroed (VEX/EVEX dest). Masked/zeroing per `writemask`. Register
    /// src only. Cold/masked → shared `exec_vpabs` (jit == interp).
    VPAbs {
        dst: u8,
        src: u8,
        elem: u8,
        dst_width: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// SSE4.1 variable blend `blendvps`/`blendvpd`/`pblendvb` (task-168.5.4): for each
    /// `lane`-byte lane, take it from `src` when the lane's most-significant bit in the
    /// implicit mask register (XMM0) is set, else keep `dst`.
    VPBlendV {
        dst: u8,
        src: u8,
        lane: u8,
    },
    /// As [`IrOp::VPBlendV`] but the blend source is a 128-bit memory operand.
    VPBlendVM {
        dst: u8,
        addr: Val,
        lane: u8,
    },
    /// SSE4.1 `round{ps,pd,ss,sd}` (task-168.5.4): round each lane (or, when `scalar`,
    /// only lane 0, keeping the rest of `dst`) per the imm8 `mode` — bits[1:0] select
    /// nearest-even/floor/ceil/truncate; bit[2] (use MXCSR) is treated as nearest-even.
    VPRound {
        dst: u8,
        src: u8,
        prec: FPrec,
        mode: u8,
        scalar: bool,
    },
    /// As [`IrOp::VPRound`] but the source is a memory operand.
    VPRoundM {
        dst: u8,
        addr: Val,
        prec: FPrec,
        mode: u8,
        scalar: bool,
    },
    /// Masked EVEX bitwise logic `vpxor{d,q}{k}{z}` etc. (task-168.5.5): compute
    /// `op(a, b)` per lane, then write it into `dst` under opmask `k` at `elem`-byte
    /// granularity — merge (keep `dst`) or, when `zeroing`, zero the masked-off elements.
    VMaskedLogic {
        dst: u8,
        a: u8,
        b: u8,
        op: VLogicOp,
        k: u8,
        elem: u8,
        zeroing: bool,
        bytes: u16,
    },
    /// EVEX masked packed arithmetic `vp{add,sub,min,max,mull}{b,w,d,q}{k}{z}` (task-
    /// 168.5.5): compute the packed op per `elem`-byte lane, then merge/zero-mask the
    /// result under `k`. Register src2 only (masked mem-src deferred). Cold + masked →
    /// shared `exec_masked_packed` helper (jit == interp), like [`VMaskedLogic`].
    VMaskedPacked {
        dst: u8,
        a: u8,
        b: u8,
        op: PackedBinOp,
        k: u8,
        elem: u8,
        zeroing: bool,
        bytes: u16,
    },
    /// EVEX `vpternlog{d,q}` (task-168.5.2): 3-input arbitrary bitwise logic over `bytes`.
    /// Each result bit is `imm8[(a<<2)|(b<<1)|c]` where `a`/`b`/`c` are the corresponding
    /// bits of the three sources; `dst` is both the first source and the destination.
    /// Register src3 only; masked forms are deferred.
    VPTernlog {
        dst: u8,
        b: u8,
        c: u8,
        imm: u8,
        bytes: u16,
    },
    /// Memory-source `src3` form of [`IrOp::VPTernlog`] (task-195): `c` is a `bytes`-wide
    /// vector at `addr` (`vpternlogd ymm, ymm, [mem], imm8`).
    VPTernlogM {
        dst: u8,
        b: u8,
        addr: Val,
        imm: u8,
        bytes: u16,
    },
    /// 256-bit packed integer arithmetic per `lane` bytes, both halves.
    VPackedBin256 {
        dst: u8,
        a: u8,
        b: u8,
        lane: u8,
        op: PackedBinOp,
    },
    /// 256-bit packed arithmetic with a 32-byte memory source.
    VPackedBin256M {
        dst: u8,
        a: u8,
        addr: Val,
        lane: u8,
        op: PackedBinOp,
    },
    /// Width-generic EVEX packed integer arithmetic `dst = a OP b` over `bytes` (16/32/64)
    /// per `lane`-byte element (task-168.5/195) — the 512-bit `vpaddq`/`vpsubb`/… glibc
    /// uses. Writes clear the register above `bytes`. Register src2; masked forms deferred.
    VPackedWide {
        dst: u8,
        a: u8,
        b: u8,
        lane: u8,
        op: PackedBinOp,
        bytes: u16,
    },
    /// Memory-source `src2` form of [`IrOp::VPackedWide`] (task-195): `b` is a `bytes`-wide
    /// vector at `addr` (`vpaddq zmm, zmm, [mem]`).
    VPackedWideM {
        dst: u8,
        a: u8,
        addr: Val,
        lane: u8,
        op: PackedBinOp,
        bytes: u16,
    },
    /// `vpmovmskb` on a YMM: a 32-bit mask of the top bit of each of 32 bytes.
    VMoveMaskB256 {
        dst: Temp,
        src: u8,
    },

    // --- AVX2 specials (task-168.3). ---
    /// `vpbroadcast{b,w,d,q}`: replicate the low `elem`-byte element of XMM `src`
    /// across `dst`. `w256` fills the full YMM; else the XMM (upper 128 zeroed).
    VBroadcast {
        dst: u8,
        src: u8,
        elem: u8,
        w256: bool,
    },
    /// `vpbroadcast{b,w,d,q}` from a memory scalar at `addr`.
    VBroadcastM {
        dst: u8,
        addr: Val,
        elem: u8,
        w256: bool,
    },
    /// EVEX `vpbroadcast{b,w,d,q}` from a GPR: replicate the low `elem`-byte value of
    /// `src` across the low `width` bytes of `dst` (16/32/64), zeroing above `width`
    /// (unmasked, task-168.5).
    VBroadcastGpr {
        dst: u8,
        src: Val,
        elem: u8,
        width: u16,
    },
    /// EVEX `vpcmp{b,w,d,q}` / `vpcmpu{b,w,d,q}` → opmask (task-168.5). Compares the
    /// `elem`-byte lanes of vectors `a` and `b` across the low `width` bytes with
    /// predicate `pred` (0=EQ 1=LT 2=LE 3=FALSE 4=NE 5=GE 6=GT 7=TRUE), signed vs
    /// unsigned per `signed`, and writes one bit per lane into opmask `k` (unmasked).
    VPCmpToMask {
        k: u8,
        a: u8,
        b: u8,
        elem: u8,
        width: u16,
        pred: u8,
        signed: bool,
        /// EVEX write-mask k1–k7: result bits are ANDed with it (only those lanes are
        /// compared; the rest are zeroed). `None` = unmasked (k0).
        writemask: Option<u8>,
    },
    /// Memory-source `src2` form of [`IrOp::VPCmpToMask`] (task-195): `b` is a `width`-byte
    /// vector loaded from `addr`. glibc's AVX-512 string/memcmp routines fold the second
    /// operand as a memory load (`vpcmpeqb k, zmm, [rsi]`).
    VPCmpToMaskM {
        k: u8,
        a: u8,
        addr: Val,
        elem: u8,
        width: u16,
        pred: u8,
        signed: bool,
        writemask: Option<u8>,
    },
    /// EVEX `vptestm{b,w,d,q}` / `vptestnm{b,w,d,q}` → opmask (task-168.5.4): per
    /// `elem`-byte lane over the low `width` bytes, `k[i] = (a[i] & b[i]) != 0`, or
    /// `== 0` when `neg` (the `nm` "not-mask" form — glibc's AVX-512 strlen tests for
    /// zero bytes). Result ANDed with the write-mask. `#DE` etc. unaffected.
    VPTestToMask {
        k: u8,
        a: u8,
        b: u8,
        elem: u8,
        width: u16,
        neg: bool,
        writemask: Option<u8>,
    },
    /// Memory-source `src2` form of [`IrOp::VPTestToMask`] (task-195): `b` is a `width`-byte
    /// vector loaded from `addr`.
    VPTestToMaskM {
        k: u8,
        a: u8,
        addr: Val,
        elem: u8,
        width: u16,
        neg: bool,
        writemask: Option<u8>,
    },
    /// `kortest{b,w,d,q}`: `t = k[a] | k[b]` over `width` bits; `ZF = (t == 0)`,
    /// `CF = (t == all-ones)`, other flags cleared (task-168.5 opmask subsystem).
    VKOrTest {
        a: u8,
        b: u8,
        width: u8,
    },
    /// `kmov{b,w,d,q}` GPR/immediate → opmask `k` (low `width` bits kept).
    VKFromGpr {
        k: u8,
        src: Val,
        width: u8,
    },
    /// `kmov{b,w,d,q}` opmask `k` → GPR `dst` (zero-extended, low `width` bits).
    VKToGpr {
        dst: Temp,
        k: u8,
        width: u8,
    },
    /// `kmov{b,w,d,q}` opmask → opmask (`dst = src` over `width` bits).
    VKMovKK {
        dst: u8,
        src: u8,
        width: u8,
    },
    /// `kunpck{bw,wd,dq}` (task-195): interleave two opmasks — `k[dst] = (k[a]_low <<
    /// half) | k[b]_low`, keeping the low `half` bits of each (`half` = 8/16/32). glibc's
    /// AVX-512 routines build a wide mask from two narrow compares this way.
    VKUnpack {
        dst: u8,
        a: u8,
        b: u8,
        half: u8,
    },
    /// `k{or,and,andn,xor,xnor}{b,w,d,q}` (task-195): bitwise op on two opmasks over
    /// the low `width` bits (8/16/32/64), high bits cleared. glibc's AVX-512 string
    /// routines combine per-chunk compare masks with these (`kord`, `korb`, …).
    VKBinOp {
        dst: u8,
        a: u8,
        b: u8,
        op: VKLogicOp,
        width: u8,
    },
    /// `knot{b,w,d,q}` (task-195): `k[dst] = ~k[a]` over the low `width` bits.
    VKNot {
        dst: u8,
        a: u8,
        width: u8,
    },
    /// `kshift{l,r}{b,w,d,q}` (task-195): shift opmask `a` left/right by `amount` bits
    /// within the low `width` bits (`left` = shift-left). Shifts ≥ `width` clear the mask.
    VKShift {
        dst: u8,
        a: u8,
        amount: u8,
        width: u8,
        left: bool,
    },
    /// EVEX narrowing move `vpmov{q,d,w}{d,w,b}` (task-195): truncate each `from`-byte
    /// src lane to its low `to` bytes and pack contiguously into dst's low lanes; bits
    /// above the packed result are zeroed (EVEX dest). Masked/zeroing per `writemask`
    /// at `to` granularity. Register dst only (memory dst deferred). Cold → shared
    /// `exec_vpmov_narrow` (jit == interp).
    VPmovNarrow {
        dst: u8,
        src: u8,
        from: u8,
        to: u8,
        src_width: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// EVEX narrowing move to a **memory** destination `vpmov{q,d,w}{d,w,b} [addr], src`
    /// (task-195, unmasked): truncate each `from`-byte source lane to `to` bytes and store
    /// them contiguously at `addr`. A store fault traps like any vector store. Masked
    /// memory-dest forms (per-lane fault suppression) are deferred.
    VPmovNarrowMem {
        src: u8,
        addr: Val,
        from: u8,
        to: u8,
        src_width: u16,
    },
    /// `vpermt2{b,w,d,q}` (task-195): two-table cross-lane permute. For each `elem`-byte
    /// lane, `idx` selects one of the `2*(bytes/elem)` lanes across the concatenation of
    /// `dst`'s old value (table 0) and `tbl` (table 1); the picked lane overwrites `dst`.
    /// Masked/zeroing per `writemask`. glibc/coreutils build shuffle tables with it.
    /// Cold + masked → executed by the shared `exec_vpermt2` helper (jit == interp).
    VPermT2 {
        dst: u8,
        idx: u8,
        tbl: u8,
        elem: u8,
        writemask: Option<u8>,
        zeroing: bool,
        bytes: u16,
    },
    /// `vinserti128`/`vinsertf128`: `dst` = YMM `src` with its `hi`-selected 128-bit
    /// lane replaced by XMM `ins`.
    VInsert128 {
        dst: u8,
        src: u8,
        ins: u8,
        hi: bool,
    },
    /// `vextracti128`/`vextractf128`: XMM `dst` = the `hi`-selected 128-bit lane of
    /// YMM `src`.
    VExtract128 {
        dst: u8,
        src: u8,
        hi: bool,
    },
    /// 256-bit `vpshufb`: per-128-lane byte shuffle, `dst = pshufb(a, idx)` on each
    /// half independently (task-168.3).
    VPshufb256 {
        dst: u8,
        a: u8,
        idx: u8,
    },
    /// 256-bit `vpshufb` with a 32-byte memory index.
    VPshufb256M {
        dst: u8,
        a: u8,
        addr: Val,
    },
    /// 256-bit packed shift by immediate on both halves (task-168.3).
    VPackedShift256 {
        dst: u8,
        a: u8,
        imm: u8,
        lane: u8,
        right: bool,
        arith: bool,
    },
    /// `vpermq`: permute the four 64-bit quadwords of the 256-bit `src` across the
    /// full register, `dst[i] = src[(imm >> 2i) & 3]` (cross-lane, task-168.3).
    VPermq {
        dst: u8,
        src: u8,
        imm: u8,
    },
    /// `vpermd`: cross-lane 32-bit gather over 8 dwords, `dst[i] = src[ctrl[i] & 7]`
    /// where `ctrl` supplies the per-lane indices (task-168.3).
    VPermd {
        dst: u8,
        ctrl: u8,
        src: u8,
    },
    /// `vperm2i128`/`vperm2f128`: select each 128-bit output lane from the four
    /// input halves {a.lo, a.hi, b.lo, b.hi}; imm bit 3/7 zeroes the lo/hi lane.
    VPerm2i128 {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
    },
    /// 256-bit `vpalignr`: per-128-lane byte concatenate-and-shift, applied to the
    /// low and high halves independently (task-168.3).
    VPalignr256 {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
    },
    /// `vptest`/`ptest`: `ZF = (b & a == 0)`, `CF = (b & !a == 0)` over the full
    /// width (`a` = DEST/op0, `b` = SRC/op1); OF/SF/AF/PF cleared. `w256` selects
    /// the 256-bit form (task-168.4). Writes flags only, no vector register.
    VPtest {
        a: u8,
        b: u8,
        w256: bool,
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
    // cvt{,u}si2s{s,d}: `int_size`-byte integer `src` -> float in `dst`'s low lane,
    // preserving the upper bytes. `signed` selects `cvtsi2s*` vs the AVX-512 unsigned
    // `cvtusi2s*` form (task-195).
    VCvtFromInt {
        dst: u8,
        src: Val,
        int_size: u8,
        prec: FPrec,
        signed: bool,
    },
    // cvt(t)s{s,d}2{si,usi}: `prec`-wide float `src` (raw bits) -> `int_size`-byte integer
    // in `dst`. `trunc` = toward zero (cvtt*), else round to nearest even. `signed` selects
    // the signed (`*2si`) vs the AVX-512 unsigned (`*2usi`) form (task-195).
    VCvtToInt {
        dst: Temp,
        src: Val,
        int_size: u8,
        prec: FPrec,
        trunc: bool,
        signed: bool,
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
        /// Stack-frame width in bytes: 8 in long mode, 4 for a 32-bit push of the
        /// return address (Compat32 default operand size), 2 under a 66h override.
        slot: u8,
        /// Truncate the stack pointer to 32 bits after adjusting it (Compat32: ESP
        /// wraps mod 2^32; the return address stored is already truncated at lift).
        wrap_sp: bool,
    },
    Ret {
        /// Bytes popped for the return address (8 long, 4 Compat32, 2 under 66h).
        slot: u8,
        /// Extra bytes added to the stack pointer after the pop (`ret imm16`).
        pop_extra: u16,
        /// Truncate the stack pointer and the popped EIP to 32 bits (Compat32).
        wrap_sp: bool,
    },
    Syscall,
    Hlt,
    /// A guest CPU exception raised *by the instruction itself* (not a lift gap and
    /// not a memory fault): `ud2` → `#UD` (vector 6), `int3` → `#BP` (3), `int1` →
    /// `#DB` (1). Ends the block and surfaces `Exit::Exception { vector, addr }`.
    ///
    /// `advance` is the x86 saved-RIP delta, following the fault/trap distinction
    /// (portable — it models the architecture, not the host): a **fault** (`#UD`)
    /// leaves RIP on the instruction (`advance = 0`); a **trap** (`#BP`, `#DB`)
    /// resumes *past* it (`advance = instruction length`). `#DE` (div, vector 0) is a
    /// fault with its own path (`IrOp::Divide`) and does not go through here.
    Trap {
        vector: u8,
        advance: u8,
    },
    /// A port-I/O instruction (`in`/`out`, imm8 or `dx` form) — a trap-out to the
    /// embedder (§5.2), the machine counterpart of MMIO. Ends the block with RIP
    /// *past* the instruction (like `Syscall`), surfacing `Exit::PortIo`. For `out`,
    /// `value` carries the accumulator (`al`/`ax`/`eax`) contents; for `in`, `value`
    /// is unused and the embedder answers by writing the result into the accumulator
    /// via `Vcpu::complete_port_in` (which honours 32-bit-zero / 16/8-bit-merge).
    /// `port` is the 16-bit port (`Imm` for imm8, a `Temp` holding `dx` otherwise);
    /// `size` is the access width (1/2/4). `ins`/`outs` are deliberately rejected as
    /// `UnknownInstruction` (see `lift.rs`).
    PortIo {
        port: Val,
        value: Val,
        size: u8,
        dir_out: bool,
    },
}

/// Bitwise vector logic op (§3.1 M8).
#[derive(Copy, Clone, Debug)]
pub enum VLogicOp {
    Xor,
    And,
    Or,
    Andn,
}

/// Bitwise op for the opmask logical family `k{or,and,andn,xor,xnor}{b,w,d,q}`
/// (task-195). Distinct from [`VLogicOp`] because opmasks add `Xnor` (glibc uses
/// `kxnor k,k,k` as an all-ones idiom) and never carry `Andn`'s vector width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VKLogicOp {
    Or,
    And,
    Andn,
    Xor,
    Xnor,
}

/// Bit-scan family (task-176). `Bsf`/`Bsr` are the SSE-era scans (only ZF defined,
/// destination preserved on a zero source); `Tzcnt`/`Lzcnt` are the BMI1/v3 counts,
/// defined on zero (= operand bit-width) and setting ZF (result==0) + CF (src==0).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BitScanOp {
    Bsf,
    Bsr,
    Tzcnt,
    Lzcnt,
}

/// BMI1/BMI2 single-destination bit op (task-168.5.3). One `IrOp::Bmi` carries the
/// variant + `size`; each computes its result and flags per Intel. `a` is the primary
/// source, `b` the secondary (control/mask; ignored by the unary bls* ops). `pdep`/
/// `pext` (need a helper — no native Cranelift op) and `mulx` (two destinations) are
/// handled separately, not here.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BmiOp {
    /// `andn`: `~a & b`. SF/ZF per result, CF=OF=0.
    Andn,
    /// `blsi`: `a & -a` (isolate lowest set bit). CF = (a != 0).
    Blsi,
    /// `blsr`: `a & (a - 1)` (clear lowest set bit). CF = (a == 0).
    Blsr,
    /// `blsmsk`: `a ^ (a - 1)` (mask up to lowest set bit). CF = (a == 0).
    Blsmsk,
    /// `bextr`: extract `b[15:8]` bits of `a` starting at bit `b[7:0]`. ZF per result.
    Bextr,
    /// `bzhi`: zero bits of `a` from index `b[7:0]` up. CF = (index > width-1).
    Bzhi,
    /// `pdep`: deposit the contiguous low bits of `a` into the positions set in mask
    /// `b`. Sets NO flags.
    Pdep,
    /// `pext`: extract the bits of `a` at the positions set in mask `b`, packed low.
    /// Sets NO flags.
    Pext,
}

impl BmiOp {
    /// `pdep`/`pext` leave the flags untouched; the rest set CF/ZF/SF (OF=0).
    pub fn writes_flags(self) -> bool {
        !matches!(self, BmiOp::Pdep | BmiOp::Pext)
    }
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
    /// `pmulld` — per-lane low 32 bits of the 32×32 product.
    MulLo32,
    /// `vpmullq` (AVX-512DQ) — per-lane low 64 bits of the 64×64 product.
    MulLo64,
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
    /// Reverse subtract: `new = src - old`. Used for atomic `lock neg` (`src = 0`);
    /// no host has a native reverse-subtract atomic, so both backends emit a CAS
    /// loop for it.
    Rsub,
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
/// rest. Sub-blocks may be non-contiguous, so SMC invalidation uses [`Self::spans`].
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
