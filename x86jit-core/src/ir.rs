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
    /// `vpblendw` (VEX.128, task-195): per 16-bit word lane, take it from `b` when the
    /// corresponding `imm8` bit is set, else from `a`; bits 255:128 cleared. Register src.
    VBlendW {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
    },
    /// `vpblendd` — per-dword immediate blend over `bytes` (16/32): dword `i` is taken
    /// from `b` when `imm8[i]` is set, else from `a` (task-215). VEX form clears bits
    /// above `bytes`.
    VBlendD {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
        bytes: u16,
    },
    /// FMA3 fused multiply-add `vf[n]m{add,sub}{132,213,231}{ss,sd,ps,pd}` (task-201):
    /// per lane `dst = ±(x*y) ± z` with a SINGLE rounding. The 132/213/231 operand order
    /// is resolved at lift time into the `x`/`y`/`z` register roles; `neg_prod`/`neg_add`
    /// pick the `vfnm`/`vf*sub` sign. `scalar` = low-element only (upper of dst preserved,
    /// 255:128 cleared); else packed over `bytes`. Register src. Cold → shared `exec_fma`.
    VFma {
        dst: u8,
        x: u8,
        y: u8,
        z: u8,
        prec: FPrec,
        scalar: bool,
        neg_prod: bool,
        neg_add: bool,
        bytes: u16,
        /// EVEX write-mask k-register (`None` = unmasked VEX/EVEX-k0); masks at `prec`
        /// element granularity (task-201 AC#3).
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// As [`VFma`] but one source (`mem_role`: 0=x, 1=y, 2=z) is a memory operand `[addr]`
    /// — the FMA3 memory form always puts it in op2 (task-201). A load fault traps.
    VFmaM {
        dst: u8,
        x: u8,
        y: u8,
        z: u8,
        addr: Val,
        mem_role: u8,
        prec: FPrec,
        scalar: bool,
        neg_prod: bool,
        neg_add: bool,
        bytes: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// AES-NI round op `aes{enc,dec}{,last}` (SSE + VEX.128, task-205): `dst = f(a, b)`
    /// where `f` is picked by `op` — `a` is the state, `b` the round key. The SSE form
    /// is in-place (`a == dst`); the VEX 3-operand form passes op1 as `a` and reads both
    /// `a` and `b` before writing `dst`, so a VEX `b`/`dst` alias is safe (no pre-copy).
    /// VEX zeroes bits 255:128 via a following `VZeroUpper`. Cold → shared `aes.rs`.
    VAes {
        dst: u8,
        a: u8,
        b: u8,
        op: AesOp,
    },
    /// As [`VAes`] but the round key `b` is a memory operand `[addr]`. Load fault traps.
    VAesM {
        dst: u8,
        a: u8,
        addr: Val,
        op: AesOp,
    },
    /// `aesimc dst, src` (SSE + VEX.128, task-205): `dst = InvMixColumns(src)` — single
    /// source, no XOR. VEX zeroes 255:128 via a following `VZeroUpper`.
    VAesImc {
        dst: u8,
        src: u8,
    },
    /// As [`VAesImc`] with a memory source `[addr]`. Load fault traps.
    VAesImcM {
        dst: u8,
        addr: Val,
    },
    /// `aeskeygenassist dst, src, imm8` (SSE + VEX.128, task-205): SubWord/RotWord/RCON
    /// per Intel SDM (`imm8` = RCON). VEX zeroes 255:128 via a following `VZeroUpper`.
    VAesKeygen {
        dst: u8,
        src: u8,
        imm: u8,
    },
    /// As [`VAesKeygen`] with a memory source `[addr]`. Load fault traps.
    VAesKeygenM {
        dst: u8,
        addr: Val,
        imm: u8,
    },
    /// SHA-NI op `sha{256,1}...` (SSE, task-207): `dst = f(dst, src[, xmm0/imm])`.
    /// Two-source register form. `a` = op1 (== dst), `b` = op2. `sha256rnds2` reads
    /// xmm0 implicitly (`ShaOp::Sha256Rnds2`); `sha1rnds4` folds `imm8[1:0]` into
    /// the op selection. Cold → shared `sha.rs` primitives.
    VSha {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
        op: ShaOp,
    },
    /// As [`VSha`] but op2 is a memory operand `[addr]`. Load fault traps.
    VShaM {
        dst: u8,
        a: u8,
        addr: Val,
        imm: u8,
        op: ShaOp,
    },
    /// GFNI op `gf2p8{mulb,affineqb,affineinvqb}` (SSE + VEX.128, task-210): `dst =
    /// f(a, b[, imm8])`. `a` = op1 (state / affine input), `b` = op2 (multiplier /
    /// affine matrix), `imm` = affine constant (0 for mulb). The SSE form is in-place
    /// (`a == dst`); the VEX 3-operand form passes op1 as `a` and reads both sources
    /// before writing `dst`, so a VEX `b`/`dst` alias is safe (no pre-copy). VEX zeroes
    /// bits 255:128 via a following `VZeroUpper`. Cold → shared `gfni.rs`.
    VGfni {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
        op: GfniOp,
    },
    /// As [`VGfni`] but op2 is a memory operand `[addr]`. Load fault traps.
    VGfniM {
        dst: u8,
        a: u8,
        addr: Val,
        imm: u8,
        op: GfniOp,
    },
    /// `movq2dq xmm, mm` (SSE2, task-208): copy MMX register `src_mm` (= low 64 bits of
    /// physical `fpr[src_mm]`) into the low 64 bits of `dst`, zeroing the upper 64.
    Movq2dq {
        dst: u8,
        src_mm: u8,
    },
    /// `movdq2q mm, xmm` (SSE2, task-208): copy the low 64 bits of `src_xmm` into MMX
    /// register `dst_mm` (= low 64 bits of physical `fpr[dst_mm]`; the x87 exponent field
    /// is set all-ones, as writing an MMX register does on hardware).
    Movdq2q {
        dst_mm: u8,
        src_xmm: u8,
    },
    /// `pclmulqdq dst, a, b, imm8` (PCLMULQDQ, task-211): carry-less GF(2)[x] product of
    /// two `imm8`-selected 64-bit halves → full 128-bit. `a` = op1, `b` = op2; `imm8[0]`
    /// selects `a`'s half, `imm8[4]` selects `b`'s half. The SSE form is in-place
    /// (`a == dst`); the VEX 3-operand form passes op1 as `a` and reads both sources
    /// before writing `dst`, so a VEX `b`/`dst` alias is safe (no pre-copy). VEX zeroes
    /// bits 255:128 via a following `VZeroUpper`. Cold → shared `pclmul.rs`.
    VPclmul {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
    },
    /// As [`VPclmul`] but op2 `b` is a memory operand `[addr]`. Load fault traps.
    VPclmulM {
        dst: u8,
        a: u8,
        addr: Val,
        imm: u8,
    },
    /// SSSE3 `psign{b,w,d}` (SSE + VEX.128, task-210): per `lane`-byte element
    /// (1/2/4), `dst[i] = ctrl[i] < 0 ? -src[i] : (ctrl[i] == 0 ? 0 : src[i])`.
    /// `a` = src (op1/dst for SSE, op1 for VEX), `b` = ctrl (op2). Reads both sources
    /// before writing dst → a VEX `b`/`dst` alias is safe. VEX zeroes bits 255:128.
    /// Pure element-wise codegen (no helper).
    VPsign {
        dst: u8,
        a: u8,
        b: u8,
        lane: u8,
    },
    /// As [`VPsign`] but the control operand is a memory operand `[addr]`. Load fault traps.
    VPsignM {
        dst: u8,
        a: u8,
        addr: Val,
        lane: u8,
    },
    /// Pack `pack{ss,us}{wb,dw}` (SSE/VEX/EVEX, task-195): saturate each `from_elem`-byte
    /// source lane (always read signed) to a `from_elem/2`-byte lane — `signed` picks the
    /// signed vs unsigned saturation range — packing `a`'s lanes low and `b`'s high within
    /// each 128-bit lane, over `bytes`. Register src. Cold → shared `exec_vpack`.
    VPackWide {
        dst: u8,
        a: u8,
        b: u8,
        from_elem: u8,
        signed: bool,
        bytes: u16,
    },
    /// `pmaddwd` (SSE2, task-190): multiply the 8 signed 16-bit lanes of `a` by the 8
    /// signed 16-bit lanes of `b` pairwise, then add adjacent products into 4 signed
    /// 32-bit dwords (`dst.dword[i] = a.word[2i]*b.word[2i] + a.word[2i+1]*b.word[2i+1]`,
    /// two's-complement wrap on the one overflowing case). Register src. Cold → shared
    /// `exec_pmaddwd` (jit == interp).
    VPMAddWd {
        dst: u8,
        a: u8,
        b: u8,
    },
    /// EVEX/VEX-256 `vpshufd` (task-195): per-128-bit-lane dword shuffle by `imm8` over
    /// `bytes` (any width), dword-granularity masking; bits above `bytes` zeroed (EVEX
    /// dest). Register src only. Cold/masked → shared `exec_vshuffle32_wide`.
    VShuffle32Wide {
        dst: u8,
        a: u8,
        imm: u8,
        bytes: u16,
        writemask: Option<u8>,
        zeroing: bool,
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
    // pshufb (SSSE3): `dst[i] = (idx[i] & 0x80) ? 0 : a[i's low nibble of idx]`.
    // `a` is the data source (dst for the in-place SSE form, op1 for the 3-operand
    // VEX form) — kept explicit so a VEX `idx` that aliases `dst` isn't clobbered by a
    // pre-copy (task-203). Index vector from a register or memory (`VPshufbM`).
    VPshufb {
        dst: u8,
        a: u8,
        idx: u8,
    },
    VPshufbM {
        dst: u8,
        addr: Val,
    },
    /// EVEX `vpshufb` (task-195): per-128-bit-lane byte shuffle `dst = pshufb(a, idx)`
    /// over `bytes` (any width), with byte-granularity masking. Register idx only; bits
    /// above `bytes` zeroed (EVEX dest). Cold/masked → shared `exec_vpshufb_wide`.
    VPshufbWide {
        dst: u8,
        a: u8,
        idx: u8,
        bytes: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    // palignr (SSSE3): concatenate `a` (high 16 bytes) with the source (low 16),
    // shift the 32-byte value right by `imm` bytes, keep the low 16. `a` is dst for
    // the in-place SSE form, op1 for the 3-operand VEX form — explicit so a VEX `src`
    // aliasing `dst` isn't clobbered by a pre-copy (task-203). Source from a register
    // (`VAlignr`) or memory (`VAlignrM`).
    VAlignr {
        dst: u8,
        a: u8,
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
    /// `vzeroupper`/`vzeroall`: zero the upper bits (255:128, and 511:256 of ZMM) of
    /// every vector register 0–15. `clear_low` additionally zeros the low 128 bits
    /// (xmm) — the difference between `vzeroall` (whole register) and `vzeroupper`
    /// (uppers only, low 128 preserved).
    VZeroUpperAll {
        clear_low: bool,
    },

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
    /// As [`VExtractLaneWide`] but the destination is memory `[addr]` (task-215): store the
    /// `idx`-th group of `num_lanes` 128-bit lanes of `src` to `[addr]`. `num_lanes` is 1
    /// (128-bit / 16 bytes, e.g. `vextracti32x4 [mem],zmm,imm`) or 2 (256-bit / 32 bytes).
    /// A fault on the store traps like any vector store.
    VExtractLaneWideM {
        src: u8,
        addr: Val,
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
    /// SSE4.2 `pcmpistrm`/`pcmpestrm` (task-195): the same string-compare aggregation as
    /// [`VPcmpStr`] but the per-element result is written as a **mask** to XMM0 (not an index
    /// to ECX). `imm[6]` selects byte/word mask vs bit mask; the same CF/ZF/SF/OF flags are
    /// set. `explicit` selects the explicit-length (`pcmpestrm`, lengths from EAX/EDX) form.
    /// `b` is a register.
    VPcmpStrMask {
        a: u8,
        b: u8,
        imm: u8,
        explicit: bool,
    },
    /// As [`VPcmpStrMask`] but source 2 is a memory operand `[addr]` loaded as a 128-bit
    /// value (task-195). A fault on the load traps like any vector load.
    VPcmpStrMaskM {
        a: u8,
        addr: Val,
        imm: u8,
        explicit: bool,
    },
    /// SSE4.1 `insertps xmm, xmm, imm8` (task-195): insert `src.dword[imm[7:6]]` into
    /// `dst.dword[imm[5:4]]`, then zero each dword `i` with `imm[i]` set. Only the low 128
    /// bits change (legacy SSE preserves 255:128). Register source.
    VInsertPs {
        dst: u8,
        src: u8,
        imm: u8,
    },
    /// As [`VInsertPs`] but the inserted dword is loaded from `[addr]` (m32 form, task-195):
    /// the `imm[7:6]` source-lane select is ignored (the memory dword is the source). A
    /// fault on the load traps like any vector load.
    VInsertPsM {
        dst: u8,
        addr: Val,
        imm: u8,
    },
    /// SSE4.1 `dpps xmm, xmm, imm8` (task-195): single-precision dot product. `imm[7:4]`
    /// masks the four `a[i]*b[i]` products entering the sum; `imm[3:0]` selects which result
    /// dwords receive the broadcast sum. `dst` is also source 1. Register source 2. Only the
    /// low 128 bits change. Cold + horizontal FP → shared helper (jit == interp).
    VDpps {
        dst: u8,
        b: u8,
        imm: u8,
    },
    /// As [`VDpps`] but source 2 is a memory operand `[addr]` loaded as 128 bits (task-195).
    /// A fault on the load traps like any vector load.
    VDppsM {
        dst: u8,
        addr: Val,
        imm: u8,
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
    /// Masked EVEX unary lane op (task-209): per `elem`-byte lane (4=d/32-bit, 8=q/64-bit)
    /// `dst = f(src)`, where `f` is `op` — leading-zero count (`vplzcnt`), rotate-left by
    /// `imm` (`vprol`), or conflict-detect (`vpconflict`, `dst[i]` = bitmask of lower lanes
    /// equal to lane `i`). Any width; bits above `dst_width` zeroed. Masked/zeroing per
    /// `writemask`. Register src only. Cold/masked → shared `exec_vp_unary_lane`.
    VpUnaryLane {
        dst: u8,
        src: u8,
        op: VpUnaryOp,
        imm: u8,
        elem: u8,
        dst_width: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// Masked EVEX blend `vpblendm{d,q}` (task-209): per `elem`-byte lane (4=d, 8=q),
    /// `dst[i] = k[i] ? b[i] : (zeroing ? 0 : a[i])`. The opmask `k` is the blend control
    /// (not a plain writemask). Any width; bits above `dst_width` zeroed. Register srcs
    /// only. Cold/masked → shared `exec_vp_blendm`.
    VpBlendm {
        dst: u8,
        a: u8,
        b: u8,
        k: u8,
        elem: u8,
        dst_width: u16,
        zeroing: bool,
    },
    /// Masked EVEX 128-bit-lane shuffle `vshuff32x4` / `vshuff64x2` (task-209): select
    /// whole 128-bit lanes from `a` (low half of dst) and `b` (high half) per `imm8`;
    /// `elem` (4/8) is only the masking granularity. `dst_width` is 256 or 512. Masked/
    /// zeroing per `writemask`. Register srcs only. Cold/masked → shared `exec_vshuf_lane`.
    VShuffLane {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
        elem: u8,
        dst_width: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// Masked EVEX `vpmultishiftqb` (AVX512-VBMI, task-209): for each qword, each output
    /// byte `i` = `data.qword` rotated right by `(ctrl.byte[i] & 63)`, low 8 bits. `ctrl`
    /// = src1, `data` = src2. Masked at byte granularity. Register srcs only. Cold/masked
    /// → shared `exec_vp_multishift`.
    VpMultishift {
        dst: u8,
        ctrl: u8,
        data: u8,
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
    /// AVX `vblendv{ps,pd}`/`vpblendvb` (task-215): the VEX 4-operand variable blend —
    /// `dst = mask-msb ? b : a` per `lane`-byte lane, with `a` (src1), `b` (src2), and the
    /// blend-control `mask` all explicit registers (unlike the SSE form's fixed XMM0 mask /
    /// dst=src1). 128-bit register form; memory src2 deferred.
    VPBlendVX {
        dst: u8,
        a: u8,
        b: u8,
        mask: u8,
        lane: u8,
    },
    /// SSE4.1 `round{ps,pd,ss,sd}` (task-168.5.4): round each lane (or, when `scalar`,
    /// only lane 0, keeping the rest of `a`) per the imm8 `mode` — bits[1:0] select
    /// nearest-even/floor/ceil/truncate; bit[2] (use MXCSR) is treated as nearest-even.
    /// `a` is the merge base (dst for SSE, op1 for the 3-operand VEX form) — explicit so
    /// a VEX `src` aliasing `dst` isn't clobbered by a pre-copy (task-203).
    VPRound {
        dst: u8,
        a: u8,
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
    /// EVEX packed shift-by-immediate over any width (128/256/512) with optional
    /// write-masking (task-215): `dst = a shift imm` per `elem`-byte lane, then merge/
    /// zero under `k`. `k == 0` is the unmasked EVEX form (full-width write, upper bits
    /// cleared). Generalizes [`IrOp::VPackedShift`]/[`IrOp::VPackedShift256`] (the VEX
    /// 128/256 paths) to ZMM + opmask, which glibc/openssl's AVX-512 crypto emits.
    VMaskedShift {
        dst: u8,
        a: u8,
        imm: u8,
        elem: u8,
        right: bool,
        arith: bool,
        k: u8,
        zeroing: bool,
        bytes: u16,
    },
    /// Packed shift by a **scalar register count** `vp{sll,srl,sra}{w,d,q} v,v,xmm`
    /// (task-215): every `elem`-byte lane of `a` is shifted by the low 64 bits of `count`'s
    /// xmm (uniform, runtime). A count ≥ the lane width yields 0 (logical/left) or the
    /// smeared sign (arithmetic right). Optional EVEX write-masking under `k`. Register count
    /// only (memory-source count deferred).
    VShiftReg {
        dst: u8,
        a: u8,
        count: u8,
        elem: u8,
        right: bool,
        arith: bool,
        k: u8,
        zeroing: bool,
        bytes: u16,
    },
    /// AVX2/AVX-512 per-element **variable** shift `vp{sll,srl,sra}v{w,d,q}` (task-215):
    /// each `elem`-byte lane of `a` is shifted by the count in the corresponding lane of
    /// `count` (unlike the imm/xmm-count shifts, the count is NOT reduced modulo the lane
    /// width — a count ≥ width yields 0 for logical/left and the smeared sign for arithmetic
    /// right). Optional EVEX write-masking under `k` (`k == 0` = unmasked, full-width write).
    /// Register count only; memory-source count is deferred. openssl's AVX-512 crypto uses
    /// `vpsllvd/vpsrlvd zmm,zmm,zmm`.
    VShiftVar {
        dst: u8,
        a: u8,
        count: u8,
        elem: u8,
        right: bool,
        arith: bool,
        k: u8,
        zeroing: bool,
        bytes: u16,
    },
    /// GFNI `gf2p8affineqb` / `gf2p8affineinvqb` / `gf2p8mulb` (task-215): per-byte
    /// operations in GF(2⁸) with the AES reduction polynomial. `mode` = 0 affine, 1 affine-
    /// of-inverse, 2 multiply. For affine, `b`'s qword is the 8×8 bit matrix applied to each
    /// byte of `a` (with `imm` the XOR constant); for multiply, `b` is the per-byte
    /// multiplier. Any width (128/256/512) + optional EVEX byte-granular write-masking.
    /// Register src2 only (memory-source deferred). openssl's vectorized AES uses these.
    VGf2p8 {
        dst: u8,
        a: u8,
        b: u8,
        imm: u8,
        mode: u8,
        k: u8,
        zeroing: bool,
        bytes: u16,
    },
    /// As [`VGf2p8`] but the second source (matrix/multiplier) is a memory operand `[addr]`
    /// (task-215): handles openssl's `vgf2p8affineqb ymm,ymm,[rip+matrix]` including the
    /// `dst == src1` aliasing case that the load-into-dst lowering can't. A fault on the load
    /// traps like any vector load.
    VGf2p8M {
        dst: u8,
        a: u8,
        addr: Val,
        imm: u8,
        mode: u8,
        k: u8,
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
    /// EVEX lane broadcast `vbroadcast{i,f}{32x2,32x4,32x8,64x2,64x4,128}` (task-214):
    /// replicate the low `chunk`-byte block (8/16/32) of vector `src` across every
    /// `chunk`-sized slot of the `dst_width`-byte dest. Masked/zeroing per `writemask` at
    /// `elem` granularity (4=`32x*`, 8=`64x*`). Register src. Cold/masked → shared helper.
    VBroadcastLane {
        dst: u8,
        src: u8,
        chunk: u8,
        elem: u8,
        dst_width: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// As [`VBroadcastLane`] but the `chunk`-byte block is a memory operand `[addr]`.
    /// Load fault traps.
    VBroadcastLaneM {
        dst: u8,
        addr: Val,
        chunk: u8,
        elem: u8,
        dst_width: u16,
        writemask: Option<u8>,
        zeroing: bool,
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
        /// `vpermi2*` (index-mode) vs `vpermt2*`: in i-mode the index is the OLD `dst`
        /// and table 0 is the `idx` operand; in t-mode the index is `idx` and table 0 is
        /// the old `dst`. Table 1 is `tbl` in both. Result overwrites `dst` (task-195).
        imode: bool,
    },
    /// As [`VPermT2`] but table 1 is a memory operand `[addr]` (task-195). A load fault
    /// traps like any vector load; `imode` selects `vpermi2`/`vpermt2` as above.
    VPermT2M {
        dst: u8,
        idx: u8,
        addr: Val,
        elem: u8,
        writemask: Option<u8>,
        zeroing: bool,
        bytes: u16,
        imode: bool,
    },
    /// Single-source cross-lane permute `vperm{d,q}` (vector-index form, task-195): for
    /// each `elem`-byte lane, `dst[i] = src[idx[i] & (n-1)]` where `n = bytes/elem` and the
    /// whole register is one table. Masked/zeroing per `writemask`. Register src only.
    /// Cold/masked → shared `exec_vperm1` (jit == interp).
    VPerm1 {
        dst: u8,
        idx: u8,
        src: u8,
        elem: u8,
        bytes: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// As [`VPerm1`] but the source table is a memory operand `[addr]` (task-215):
    /// `vpermq zmm, zmm, [mem]`. A load fault traps like any vector load. openssl's
    /// AVX-512 RSA folds the permute table as a memory operand.
    VPerm1M {
        dst: u8,
        idx: u8,
        addr: Val,
        elem: u8,
        bytes: u16,
        writemask: Option<u8>,
        zeroing: bool,
    },
    /// `vinserti128`/`vinsertf128`: `dst` = YMM `src` with its `hi`-selected 128-bit
    /// lane replaced by XMM `ins`.
    VInsert128 {
        dst: u8,
        src: u8,
        ins: u8,
        hi: bool,
    },
    /// As [`VInsert128`] but the inserted 128-bit lane comes from memory `[addr]`
    /// (task-195). A load fault traps like any vector load.
    VInsert128M {
        dst: u8,
        src: u8,
        addr: Val,
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
    // preserving `a`'s upper bytes (distinct from the zero-extending mem form). `a` is
    // the upper-bytes source (dst for SSE, op1 for the 3-operand VEX form) — explicit
    // so a VEX `src` aliasing `dst` isn't clobbered by a pre-copy (task-203).
    VFloatMov {
        dst: u8,
        a: u8,
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
    // Packed SIMD float↔int convert `cvt*p*` (task-239): read xmm `src`, write the
    // converted lanes to xmm `dst` per `kind`. Register source only — a memory operand
    // is materialised into `dst` by a preceding `VLoad`, then `src == dst` (the pshufd
    // pattern). The narrowing forms zero dst[127:64]; the whole result is written, so
    // an SSE op leaves dst[127:X] as the kind dictates and a VEX form appends VZeroUpper.
    VPackedCvt {
        dst: u8,
        src: u8,
        kind: PackedCvtKind,
    },
    // sqrts{s,d}/sqrtp{s,d}: `scalar` = lane 0 only (upper preserved from `a`), else
    // all lanes. `a` is the merge base (dst for SSE, op1 for the 3-operand VEX form) —
    // explicit so a VEX `src` aliasing `dst` isn't clobbered by a pre-copy (task-203).
    // Register source.
    VFloatUnary {
        dst: u8,
        a: u8,
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
        /// Address size in bits: 64 (default long mode), or 32 under a `67h`
        /// prefix — ESI/EDI/ECX are used and pointer arithmetic wraps mod 2^32.
        addr_bits: u8,
        /// Base to add to the DS-relative *source* pointer (RSI, i.e. the read
        /// side of movs/lods/cmps). `Imm(0)` for the default `ds:` (base 0);
        /// a `Temp` holding the FS/GS base under a segment override. ES:[RDI]
        /// (stos/scas dest, cmps second operand) is never overridable, so the
        /// destination side always uses base 0.
        seg_base: Val,
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
    /// `syscall`/`sysenter` (long mode) and the i386 `int 0x80` gate both surface
    /// `Exit::Syscall`, but differ in register side effects: the AMD64 `syscall`
    /// instruction latches RCX <- next-instruction RIP and R11 <- RFLAGS (hardware),
    /// while `int 0x80` must NOT touch RCX/R11 (its i386 ABI passes args in
    /// ECX/…). `is_amd64` = true only for the real `syscall` instruction.
    Syscall {
        is_amd64: bool,
    },
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

/// Masked EVEX unary lane function for [`IrOp::VpUnaryLane`] (task-209). The `u8`
/// discriminant is the wire value the JIT helper passes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VpUnaryOp {
    /// `vplzcnt{d,q}` — leading-zero count within the element width.
    Lzcnt = 0,
    /// `vprol{d,q}` — rotate-left each lane by the immediate.
    Rol = 1,
    /// `vpconflict{d,q}` — `dst[i]` = bitmask of lower lanes `j<i` equal to lane `i`.
    Conflict = 2,
}

impl VpUnaryOp {
    /// Reconstruct from the JIT helper's `u8` wire value.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => VpUnaryOp::Lzcnt,
            1 => VpUnaryOp::Rol,
            _ => VpUnaryOp::Conflict,
        }
    }
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
    /// `pmullw` — per-lane low 16 bits of the 16×16 product.
    MulLo16,
    /// `pmulhuw` — per-lane high 16 bits of the unsigned 16×16 product.
    MulHiU16,
    /// `pmulhw` — per-lane high 16 bits of the signed 16×16 product.
    MulHiS16,
    /// `pmulld` — per-lane low 32 bits of the 32×32 product.
    MulLo32,
    /// `vpmullq` (AVX-512DQ) — per-lane low 64 bits of the 64×64 product.
    MulLo64,
    /// `pmuludq`/`vpmuludq` — unsigned 32×32→64 product of each 64-bit lane's low
    /// dword; result is the full 64-bit product (task-215).
    MulU32,
    /// `pmuldq`/`vpmuldq` (SSE4.1/AVX-512) — signed 32×32→64 product of each 64-bit
    /// lane's low dword, sign-extended before multiply; full 64-bit product (task-215).
    MulS32,
    /// `paddsb`/`paddsw` — per-lane signed saturating add (task-190).
    AddSatS,
    /// `paddusb`/`paddusw` — per-lane unsigned saturating add (task-190).
    AddSatU,
    /// `psubsb`/`psubsw` — per-lane signed saturating subtract (task-190).
    SubSatS,
    /// `psubusb`/`psubusw` — per-lane unsigned saturating subtract (task-190).
    SubSatU,
    /// `pavgb`/`pavgw` — per-lane unsigned rounding average `(a + b + 1) >> 1` (task-190).
    AvgU,
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

/// AES-NI round variant for [`IrOp::VAes`] / [`IrOp::VAesM`] (task-205). The `u8`
/// discriminant is the wire value passed to the JIT helper.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AesOp {
    /// `aesenc`: MixColumns(ShiftRows(SubBytes(state))) XOR rk.
    Enc = 0,
    /// `aesdec`: InvMixColumns(InvShiftRows(InvSubBytes(state))) XOR rk.
    Dec = 1,
    /// `aesenclast`: ShiftRows(SubBytes(state)) XOR rk.
    EncLast = 2,
    /// `aesdeclast`: InvShiftRows(InvSubBytes(state)) XOR rk.
    DecLast = 3,
}

impl AesOp {
    /// Reconstruct from the JIT-helper wire value.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => AesOp::Enc,
            1 => AesOp::Dec,
            2 => AesOp::EncLast,
            _ => AesOp::DecLast,
        }
    }
    /// Apply the round: `f(state, rk)` on the raw 128-bit xmm patterns.
    pub fn apply(self, state: u128, rk: u128) -> u128 {
        match self {
            AesOp::Enc => crate::aes::aes_enc(state, rk),
            AesOp::Dec => crate::aes::aes_dec(state, rk),
            AesOp::EncLast => crate::aes::aes_enc_last(state, rk),
            AesOp::DecLast => crate::aes::aes_dec_last(state, rk),
        }
    }
}

/// SHA-NI variant for [`IrOp::VSha`] / [`IrOp::VShaM`] (task-207). The `u8`
/// discriminant is the wire value passed to the JIT helper.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ShaOp {
    /// `sha256rnds2 dst, src, <xmm0>` — two SHA-256 rounds (xmm0 = W+K, implicit).
    Sha256Rnds2 = 0,
    /// `sha256msg1 dst, src`.
    Sha256Msg1 = 1,
    /// `sha256msg2 dst, src`.
    Sha256Msg2 = 2,
    /// `sha1rnds4 dst, src, imm8` — four SHA-1 rounds (imm8[1:0] selects f/K).
    Sha1Rnds4 = 3,
    /// `sha1nexte dst, src`.
    Sha1NextE = 4,
    /// `sha1msg1 dst, src`.
    Sha1Msg1 = 5,
    /// `sha1msg2 dst, src`.
    Sha1Msg2 = 6,
}

impl ShaOp {
    /// Reconstruct from the JIT-helper wire value.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => ShaOp::Sha256Rnds2,
            1 => ShaOp::Sha256Msg1,
            2 => ShaOp::Sha256Msg2,
            3 => ShaOp::Sha1Rnds4,
            4 => ShaOp::Sha1NextE,
            5 => ShaOp::Sha1Msg1,
            _ => ShaOp::Sha1Msg2,
        }
    }

    /// Apply the op on the raw 128-bit xmm patterns. `a` = op1 (dst), `b` = op2,
    /// `xmm0` = the implicit W+K operand (only used by `sha256rnds2`), `imm` = the
    /// `sha1rnds4` immediate (ignored by the others).
    pub fn apply(self, a: u128, b: u128, xmm0: u128, imm: u8) -> u128 {
        match self {
            ShaOp::Sha256Rnds2 => crate::sha::sha256rnds2(a, b, xmm0),
            ShaOp::Sha256Msg1 => crate::sha::sha256msg1(a, b),
            ShaOp::Sha256Msg2 => crate::sha::sha256msg2(a, b),
            ShaOp::Sha1Rnds4 => crate::sha::sha1rnds4(a, b, imm),
            ShaOp::Sha1NextE => crate::sha::sha1nexte(a, b),
            ShaOp::Sha1Msg1 => crate::sha::sha1msg1(a, b),
            ShaOp::Sha1Msg2 => crate::sha::sha1msg2(a, b),
        }
    }
}

/// GFNI variant for [`IrOp::VGfni`] / [`IrOp::VGfniM`] (task-210). The `u8`
/// discriminant is the wire value passed to the JIT helper.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GfniOp {
    /// `gf2p8mulb`: per-byte GF(2^8) multiply mod 0x11B (ignores `imm`).
    Mulb = 0,
    /// `gf2p8affineqb`: per-byte affine transform (8x8 matrix from `b`) XOR `imm`.
    AffineQb = 1,
    /// `gf2p8affineinvqb`: `AffineQb` after mapping the input byte through the
    /// GF(2^8) multiplicative inverse.
    AffineInvQb = 2,
}

impl GfniOp {
    /// Reconstruct from the JIT-helper wire value.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => GfniOp::Mulb,
            1 => GfniOp::AffineQb,
            _ => GfniOp::AffineInvQb,
        }
    }
    /// Apply the op on the raw 128-bit xmm patterns. `a` = op1 (affine input / left
    /// multiplicand), `b` = op2 (affine matrix / right multiplicand), `imm` = affine
    /// constant (ignored by `Mulb`).
    pub fn apply(self, a: u128, b: u128, imm: u8) -> u128 {
        match self {
            GfniOp::Mulb => crate::gfni::gf2p8mulb(a, b),
            GfniOp::AffineQb => crate::gfni::gf2p8affineqb(a, b, imm),
            GfniOp::AffineInvQb => crate::gfni::gf2p8affineinvqb(a, b, imm),
        }
    }
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

/// Packed SIMD float↔int conversion (SSE2/AVX `cvt*p*`, task-239). Each variant fixes
/// the source/destination lane types, count, and rounding. Out-of-range/NaN follows the
/// same saturating (Rust `as`) convention as the scalar `VCvtToInt` path — the x86
/// integer-indefinite (`0x8000_0000`) result is deferred (see interp/codegen notes).
#[derive(Copy, Clone, Debug)]
pub enum PackedCvtKind {
    /// `cvtdq2ps`: i32×4 → f32×4.
    Dq2Ps,
    /// `cvtps2dq`: f32×4 → i32×4, round to nearest even (MXCSR default).
    Ps2Dq,
    /// `cvttps2dq`: f32×4 → i32×4, truncate toward zero.
    Tps2Dq,
    /// `cvtdq2pd`: i32×2 (low 64) → f64×2.
    Dq2Pd,
    /// `cvtps2pd`: f32×2 (low 64) → f64×2.
    Ps2Pd,
    /// `cvtpd2ps`: f64×2 → f32×2 (low 64), high 64 zeroed.
    Pd2Ps,
    /// `cvtpd2dq`: f64×2 → i32×2 (low 64), round to nearest even; high 64 zeroed.
    Pd2Dq,
    /// `cvttpd2dq`: f64×2 → i32×2 (low 64), truncate; high 64 zeroed.
    Tpd2Dq,
}

impl PackedCvtKind {
    /// Bytes read from a memory source: the pd-widening forms take m64, the rest m128.
    pub fn mem_bytes(self) -> u8 {
        match self {
            PackedCvtKind::Dq2Pd | PackedCvtKind::Ps2Pd => 8,
            _ => 16,
        }
    }
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
