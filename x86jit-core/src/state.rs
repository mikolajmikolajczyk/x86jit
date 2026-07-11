//! Guest CPU state: registers and flags (§3).

use iced_x86::Register;

/// Named guest registers exposed through the public API.
///
/// FS/GS bases are present from the start because real programs use them for
/// thread-local storage (§3.1).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Reg {
    Rax,
    Rbx,
    Rcx,
    Rdx,
    Rsi,
    Rdi,
    Rbp,
    Rsp,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
    Rip,
    FsBase,
    GsBase,
}

/// `gpr[]` slots in x86 encoding order — the inverse of [`Reg::gpr_index`].
const GPR_BY_INDEX: [Reg; 16] = [
    Reg::Rax,
    Reg::Rcx,
    Reg::Rdx,
    Reg::Rbx,
    Reg::Rsp,
    Reg::Rbp,
    Reg::Rsi,
    Reg::Rdi,
    Reg::R8,
    Reg::R9,
    Reg::R10,
    Reg::R11,
    Reg::R12,
    Reg::R13,
    Reg::R14,
    Reg::R15,
];

impl Reg {
    /// The 64-bit register occupying `gpr[]` slot `index` (x86 encoding order).
    /// Inverse of [`Reg::gpr_index`]. Panics on `index >= 16`.
    pub fn from_gpr_index(index: usize) -> Reg {
        GPR_BY_INDEX[index]
    }

    /// Index into [`CpuState::gpr`] in x86 ENCODING order (RAX=0, RCX=1, RDX=2,
    /// RBX=3, RSP=4, RBP=5, RSI=6, RDI=7, R8=8 … R15=15) — NOT this enum's
    /// declaration order. `None` for registers that aren't in `gpr[]`
    /// (`Rip`/`FsBase`/`GsBase` live in their own `CpuState` fields). (§3.1)
    ///
    /// This and [`iced_gpr_index`] are the ONE place register numbering lives.
    pub fn gpr_index(self) -> Option<usize> {
        Some(match self {
            Reg::Rax => 0,
            Reg::Rcx => 1,
            Reg::Rdx => 2,
            Reg::Rbx => 3,
            Reg::Rsp => 4,
            Reg::Rbp => 5,
            Reg::Rsi => 6,
            Reg::Rdi => 7,
            Reg::R8 => 8,
            Reg::R9 => 9,
            Reg::R10 => 10,
            Reg::R11 => 11,
            Reg::R12 => 12,
            Reg::R13 => 13,
            Reg::R14 => 14,
            Reg::R15 => 15,
            Reg::Rip | Reg::FsBase | Reg::GsBase => return None,
        })
    }
}

/// Map an iced `Register` of any width (RAX/EAX/AX/AL/AH, R8/R8D/R8W/R8L, …) to
/// its [`CpuState::gpr`] index in x86 encoding order. Sub-width and high-byte
/// registers normalize to their 64-bit parent via iced's `full_register`.
/// `None` for anything that isn't a general-purpose register (RIP, segment,
/// XMM, …). (§3.1)
///
/// iced numbers its 64-bit GPRs (`RAX`..`R15`) contiguously in encoding order,
/// so subtracting the `RAX` discriminant yields the `gpr[]` index directly. The
/// unit tests pin this mapping so an iced change can't silently break it.
pub fn iced_gpr_index(reg: Register) -> Option<usize> {
    if !reg.is_gpr() {
        return None;
    }
    Some(reg.full_register() as usize - Register::RAX as usize)
}

/// Arithmetic + direction flags (Variant A: materialized, §3.2).
///
/// `#[repr(C)]` because the JIT stores/loads individual flag fields at stable
/// offsets inside `CpuState` (§8.2.1) — same contract as `CpuState` itself.
/// One-byte bools are simple and correct; a packed RFLAGS-style `u64` bitfield
/// (fewer stores in codegen) is an M4/M5 optimization, not a day-one requirement.
///
/// Lazy flags (Variant B) are a later optimization and deliberately not modeled
/// here.
#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Flags {
    pub cf: bool,
    pub pf: bool,
    pub af: bool,
    pub zf: bool,
    pub sf: bool,
    pub of: bool,
    pub df: bool,
}

/// Flat, hot-path guest register file.
///
/// `#[repr(C)]` keeps field offsets stable so the JIT can read/write fields at
/// known offsets (§8.2.1). GPRs are indexed by x86 encoding order
/// (RAX=0, RCX=1, ...), NOT the enum's declaration order (§3.1).
#[repr(C)]
#[derive(Clone, Debug, Default)]
pub struct CpuState {
    pub gpr: [u64; 16],
    pub rip: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub flags: Flags,
    /// SSE/AVX/AVX-512 vector registers — the low 128 bits (XMM) (§3.1, M8).
    /// `#[repr(C)]` + 16-byte-aligned `u128` so the JIT loads/stores at stable
    /// offsets. 32 registers: EVEX (AVX-512) can address XMM/YMM/ZMM 16–31.
    pub xmm: [u128; 32],
    /// Bits 255:128 of each vector register (task-168.2). Legacy SSE writes leave
    /// this untouched; a VEX.128 write zeroes it; a VEX.256 write sets it;
    /// `vzeroupper` clears bits 255:128 of ZMM0–15.
    pub ymm_hi: [u128; 32],
    /// Bits 511:256 of each ZMM register (task-168.5, AVX-512). `zmm_hi[i][0]` is
    /// bits 383:256, `zmm_hi[i][1]` bits 511:384. A ZMM-width EVEX write sets these;
    /// a shorter (128/256) write zeroes them.
    pub zmm_hi: [[u128; 2]; 32],
    /// AVX-512 opmask registers k0–k7 (task-168.5). `k0` reads as all-ones when used
    /// as a write-mask (i.e. "no masking"); it is still a real, writable register.
    pub kmask: [u64; 8],
    /// x87 FPU register file (§14) as RAW 80-bit values. `ST(i)` = `fpr[(fpu_top + i) & 7]`.
    /// Raw bytes are the source of truth (not a decoded `F80`) so MMX — which aliases the
    /// low 64 bits of the *physical* registers, `MM(i)` = `fpr[i][0..8]` (task-208) — can
    /// round-trip arbitrary payloads (an `F80` decode/encode would corrupt NaN/MMX bit
    /// patterns). x87 ops decode to [`crate::f80::F80`] for arithmetic and re-encode; that
    /// round-trip is exact for the normal floats x87 produces. `fpu_top` is the stack top,
    /// `fpu_cw` the control word (round-trips through `fldcw`/`fnstcw`).
    pub fpr: [[u8; 10]; 8],
    pub fpu_top: u32,
    pub fpu_cw: u16,
    pub fpu_pad: u16,
    /// An MMIO-read value delivered by `Vcpu::complete_mmio_read`, consumed by the
    /// retried load when the block re-executes from the faulting instruction (§5.2).
    /// On `CpuState` (not `Vcpu`) so the interpreter's `Load` can see it.
    pub pending_mmio: Option<u64>,
    /// An MMIO-write acknowledgement from `Vcpu::complete_mmio_write`: the embedder
    /// performed the write's side effect, so the retried store consumes this and
    /// continues instead of re-trapping (the write counterpart of `pending_mmio`).
    pub pending_mmio_write: bool,
    /// After an `Exit::PortIo { dir: In, .. }`, the access width (1/2/4) of the port
    /// read awaiting a value. `Vcpu::complete_port_in` consumes it, merging the
    /// embedder's value into the accumulator (`al`/`ax`/`eax`) with x86 sub-register
    /// semantics. `None` when no `in` is outstanding.
    pub pending_port_in: Option<u8>,
    /// Guest CPU feature set the embedder selected (task-169). Read by `cpuid_run` and
    /// the `xgetbv` handler to project CPUID leaves / XCR0. Kept last and out of
    /// `jit_abi::CpuOffsets` — the JIT never field-loads it (only the cpuid/xgetbv
    /// helpers read it via Rust), so it needs no stable ABI offset.
    pub features: crate::features::GuestCpuFeatures,
}

impl CpuState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The full 512-bit value of vector register `reg` as four 128-bit lanes, low→high
    /// (task-170.3): `[xmm, ymm_hi, zmm_hi.0, zmm_hi.1]`. One place to gather the
    /// scattered lane arrays instead of indexing all four inline.
    #[inline]
    pub fn vec_lanes(&self, reg: usize) -> [u128; 4] {
        [
            self.xmm[reg],
            self.ymm_hi[reg],
            self.zmm_hi[reg][0],
            self.zmm_hi[reg][1],
        ]
    }

    /// Write the low `bytes` (16/32/64) of vector register `reg` from lanes `v`,
    /// zeroing everything above `bytes` — the unmasked EVEX write rule (a 128/256-bit
    /// write clears the upper bits). Centralizes the zero-upper logic that the 512-bit
    /// handlers used to open-code (task-170.3).
    #[inline]
    pub fn set_vec(&mut self, reg: usize, v: [u128; 4], bytes: u16) {
        self.xmm[reg] = v[0];
        self.ymm_hi[reg] = if bytes >= 32 { v[1] } else { 0 };
        self.zmm_hi[reg] = if bytes >= 64 { [v[2], v[3]] } else { [0, 0] };
    }

    /// AVX-512 write-masking (task-170.1, decision-13): commit `newval` into vector
    /// register `reg` under opmask `k` at `elem`-byte (1/2/4/8) granularity across the
    /// low `bytes` (16/32/64). For each lane `i`: `dst[i] = k[i] ? newval[i] :
    /// (zeroing ? 0 : dst[i])`. The single place the merge/zero rule lives — a maskable
    /// op computes its full unmasked `newval` and routes the write here instead of
    /// [`set_vec`](Self::set_vec) (which is the unmasked/k0 fast path).
    pub fn write_masked(
        &mut self,
        reg: usize,
        newval: [u128; 4],
        k: u8,
        elem: u8,
        zeroing: bool,
        bytes: u16,
    ) {
        let kmask = self.kmask[k as usize];
        let old = if zeroing {
            [0u128; 4]
        } else {
            self.vec_lanes(reg)
        };
        let bits = elem as u32 * 8;
        let lane_mask: u128 = if bits >= 128 {
            u128::MAX
        } else {
            (1u128 << bits) - 1
        };
        let per_chunk = 128 / bits; // elements in one 128-bit lane
        let chunks = (bytes as u32 / 16) as usize;
        let mut result = [0u128; 4];
        let mut e = 0u32; // running element index across the whole register
        for (chunk, slot) in result.iter_mut().enumerate() {
            if chunk >= chunks {
                break; // above `bytes` stays zero (set_vec re-asserts this)
            }
            let mut acc = 0u128;
            for j in 0..per_chunk {
                let sh = j * bits;
                let src = if (kmask >> e) & 1 != 0 {
                    newval[chunk]
                } else {
                    old[chunk]
                };
                acc |= ((src >> sh) & lane_mask) << sh;
                e += 1;
            }
            *slot = acc;
        }
        self.set_vec(reg, result, bytes);
    }

    /// Write a general-purpose register with x86 sub-register semantics (§7.1, §16
    /// — the #1 silent porting bug). `size` is the destination operand width in
    /// bytes; `index` is the `gpr[]` slot (see [`Reg::gpr_index`]).
    ///
    /// - 8: full write.
    /// - 4: write low 32 bits and **zero** bits 32–63 (`mov eax` clears the rest of rax).
    /// - 2 / 1: write low 16 / 8 bits and **preserve** the upper bits (`mov ax`/`al`).
    ///
    /// This asymmetry (4 zero-extends, 1/2 merge) is exactly the trap. High-byte
    /// registers (AH/BH/CH/DH, which write bits 8–15) are NOT expressible here —
    /// the lift rejects them rather than mis-lowering to the low byte.
    pub fn write_gpr(&mut self, index: usize, val: u64, size: u8) {
        let cur = self.gpr[index];
        self.gpr[index] = match size {
            8 => val,
            4 => val & 0xFFFF_FFFF,
            2 => (cur & !0xFFFF) | (val & 0xFFFF),
            1 => (cur & !0xFF) | (val & 0xFF),
            _ => unreachable!("gpr write size must be 1/2/4/8, got {size}"),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_gpr_index_is_encoding_order() {
        assert_eq!(Reg::Rax.gpr_index(), Some(0));
        assert_eq!(Reg::Rcx.gpr_index(), Some(1));
        assert_eq!(Reg::Rdx.gpr_index(), Some(2));
        assert_eq!(Reg::Rbx.gpr_index(), Some(3));
        assert_eq!(Reg::Rsp.gpr_index(), Some(4));
        assert_eq!(Reg::Rbp.gpr_index(), Some(5));
        assert_eq!(Reg::Rsi.gpr_index(), Some(6));
        assert_eq!(Reg::Rdi.gpr_index(), Some(7));
        assert_eq!(Reg::R8.gpr_index(), Some(8));
        assert_eq!(Reg::R15.gpr_index(), Some(15));
    }

    #[test]
    fn write_gpr_size_semantics() {
        let mut c = CpuState::new();
        c.gpr[0] = 0x1111_2222_3333_4444;
        // 4-byte write zeroes the upper 32 bits.
        c.write_gpr(0, 0xAABB_CCDD_EEFF_0011, 4);
        assert_eq!(c.gpr[0], 0x0000_0000_EEFF_0011);
        // 2-byte write preserves upper 48 bits.
        c.gpr[0] = 0x1111_2222_3333_4444;
        c.write_gpr(0, 0xFFFF_FFFF_FFFF_9999, 2);
        assert_eq!(c.gpr[0], 0x1111_2222_3333_9999);
        // 1-byte write preserves upper 56 bits.
        c.gpr[0] = 0x1111_2222_3333_4444;
        c.write_gpr(0, 0x77, 1);
        assert_eq!(c.gpr[0], 0x1111_2222_3333_4477);
        // 8-byte write replaces everything.
        c.write_gpr(0, 0xDEAD_BEEF_CAFE_BABE, 8);
        assert_eq!(c.gpr[0], 0xDEAD_BEEF_CAFE_BABE);
    }

    #[test]
    fn non_gpr_regs_have_no_index() {
        assert_eq!(Reg::Rip.gpr_index(), None);
        assert_eq!(Reg::FsBase.gpr_index(), None);
        assert_eq!(Reg::GsBase.gpr_index(), None);
    }

    #[test]
    fn iced_gpr_index_matches_encoding_order() {
        assert_eq!(iced_gpr_index(Register::RAX), Some(0));
        assert_eq!(iced_gpr_index(Register::RCX), Some(1));
        assert_eq!(iced_gpr_index(Register::RDX), Some(2));
        assert_eq!(iced_gpr_index(Register::RBX), Some(3));
        assert_eq!(iced_gpr_index(Register::RSP), Some(4));
        assert_eq!(iced_gpr_index(Register::RBP), Some(5));
        assert_eq!(iced_gpr_index(Register::RSI), Some(6));
        assert_eq!(iced_gpr_index(Register::RDI), Some(7));
        assert_eq!(iced_gpr_index(Register::R8), Some(8));
        assert_eq!(iced_gpr_index(Register::R15), Some(15));
    }

    #[test]
    fn iced_subwidth_regs_normalize_to_parent() {
        // EAX/AX/AL/AH all alias RAX -> index 0.
        assert_eq!(iced_gpr_index(Register::EAX), Some(0));
        assert_eq!(iced_gpr_index(Register::AX), Some(0));
        assert_eq!(iced_gpr_index(Register::AL), Some(0));
        assert_eq!(iced_gpr_index(Register::AH), Some(0));
        // R8D/R8W/R8L all alias R8 -> index 8.
        assert_eq!(iced_gpr_index(Register::R8D), Some(8));
        assert_eq!(iced_gpr_index(Register::R8W), Some(8));
        assert_eq!(iced_gpr_index(Register::R8L), Some(8));
        // SPL is RSP's byte -> index 4.
        assert_eq!(iced_gpr_index(Register::SPL), Some(4));
    }

    #[test]
    fn iced_non_gpr_regs_have_no_index() {
        assert_eq!(iced_gpr_index(Register::RIP), None);
        assert_eq!(iced_gpr_index(Register::FS), None);
        assert_eq!(iced_gpr_index(Register::XMM0), None);
        assert_eq!(iced_gpr_index(Register::None), None);
    }
}
