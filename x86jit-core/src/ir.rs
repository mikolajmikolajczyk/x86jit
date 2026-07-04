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
    Eq, Ne,               // ZF
    Below, BelowEq,       // CF, CF|ZF  (unsigned)
    Above, AboveEq,       // !CF&!ZF    (unsigned)
    Less, LessEq,         // SF!=OF     (signed)
    Greater, GreaterEq,   // SF==OF     (signed)
    Sign, NoSign,         // SF
    Overflow, NoOverflow, // OF
    Parity, NoParity,     // PF
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
    InsnStart { guest_addr: u64 },

    // --- data movement ---
    // NOTE: ReadReg(Reg::Rip) is FORBIDDEN — cpu.rip is stale mid-block; anything
    // that reads RIP is known statically at lift time and lowered to Imm (§6.2).
    ReadReg { dst: Temp, reg: crate::state::Reg },
    // `size` (bytes: 1/2/4/8) drives the x86 sub-register write rule centrally: a
    // 4-byte write zeroes the upper 32 bits, 1/2-byte writes preserve them (§7.1,
    // §16). rip/fs/gs writes always use size 8. Applied by CpuState::write_gpr.
    WriteReg { reg: crate::state::Reg, src: Val, size: u8 },

    // --- arithmetic / logic (size in bytes: 1,2,4,8) ---
    Add { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Sub { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    // Adc/Sbb CONSUME CF as input (a + b + CF); needed for `adc`/`sbb` and every
    // 128-bit add chain glibc/compilers emit. Flags computed over the full sum.
    Adc { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Sbb { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    And { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Or  { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Xor { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Shl { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Shr { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    // Arithmetic (sign-propagating) shift right — also lowers `cqo`/`cdq`.
    Sar { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    // Rotates. Only CF/OF are affected, and only when the masked count != 0
    // (CF_OF mask); OF is defined for count 1. (§16)
    Rol { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Ror { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    // Sign-extend `a`'s low `from` bytes to 64 bits (movsx/movsxd/cdqe).
    Sext { dst: Temp, a: Val, from: u8 },
    // Reverse the byte order of the low `size` bytes (bswap; size 4 or 8). No flags.
    Bswap { dst: Temp, a: Val, size: u8 },
    // Widening multiply: `lo`/`hi` get the low/high `size`-byte halves of the
    // `size`-width product. `signed` picks imul vs mul. CF=OF set iff the product
    // doesn't fit in the low half (SF/ZF/PF/AF undefined → CF_OF mask). (§16)
    Mul { lo: Temp, hi: Temp, a: Val, b: Val, size: u8, signed: bool, set_flags: FlagMask },
    // Divide the `size`-width `hi:lo` dividend by `divisor`: `quot`/`rem` get the
    // quotient/remainder. Raises `#DE` (a guest exception, not a lift error) on a
    // zero divisor or a quotient that overflows the destination. Flags undefined.
    Div { quot: Temp, rem: Temp, hi: Val, lo: Val, divisor: Val, size: u8, signed: bool },
    // ... Mul, Div, Rol, Ror, etc.

    // --- flags as DATA (setcc, cmovcc, adc/sbb lowering, rcl/rcr) ---
    // Materialize a condition as 0/1 (§6.2). CF alone = GetCond(Below).
    GetCond { dst: Temp, cond: Cond },

    // --- memory ---
    Load { dst: Temp, addr: Val, size: u8 },
    Store { addr: Val, src: Val, size: u8, order: MemOrder },

    // --- SIMD (SSE, §3.1 M8). XMM registers by index (0..15), 128-bit values. ---
    // Load 16/8/4 bytes from memory into xmm `dst` (upper bytes zeroed for <16).
    VLoad { dst: u8, addr: Val, size: u8 },
    VStore { addr: Val, src: u8, size: u8 },
    // xmm -> xmm move (movaps/movdqa/movdqu reg form).
    VMov { dst: u8, src: u8 },
    // movd/movq: GPR/imm -> xmm low, upper zeroed (`size` 4 or 8).
    VFromGpr { dst: u8, src: Val, size: u8 },
    // movd/movq: xmm low `size` bytes -> GPR temp.
    VToGpr { dst: Temp, src: u8, size: u8 },
    // Bitwise vector logic: pxor/pand/por/pandn (and the *ps aliases).
    VLogic { dst: u8, a: u8, b: u8, op: VLogicOp },
    // Packed integer arithmetic per `lane` bytes (1/2/4/8): padd*/psub*/pcmpeq*.
    VPackedBin { dst: u8, a: u8, b: u8, lane: u8, op: PackedBinOp },
    // Memory-source forms: `dst = op(dst, load128(addr))` (e.g. `paddd xmm,[mem]`).
    VPackedBinM { dst: u8, addr: Val, lane: u8, op: PackedBinOp },
    VLogicM { dst: u8, addr: Val, op: VLogicOp },
    // Packed logical shift of each `lane`-byte element by `imm` (psll*/psrl*).
    VPackedShift { dst: u8, a: u8, imm: u8, lane: u8, right: bool },
    // Byte-shift the whole 128-bit value right by `bytes` (psrldq).
    VByteShiftR { dst: u8, a: u8, bytes: u8 },
    // pshufd: permute the four 32-bit lanes of `a` per the imm8 selector.
    VShuffle32 { dst: u8, a: u8, imm: u8 },
    // punpckl*: interleave the low halves of `a` and `b` at `lane`-byte granularity.
    VUnpackLow { dst: u8, a: u8, b: u8, lane: u8 },
    // packuswb: pack 8+8 signed 16-bit lanes to 16 unsigned-saturated bytes.
    VPackUsWB { dst: u8, a: u8, b: u8 },
    // pinsrw: insert the low 16 bits of `src` into word lane `index` of `dst`.
    VInsertW { dst: u8, src: Val, index: u8 },

    // --- control flow: each of these ENDS the block ---
    Jump { target: Val },                              // direct: Imm, indirect: Temp
    Branch { cond: Cond, taken: u64, fallthrough: u64 }, // jcc — both targets known
    Call { target: Val, return_addr: u64 },
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
