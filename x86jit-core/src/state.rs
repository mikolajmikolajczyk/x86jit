//! Guest CPU state: registers and flags (§3).

use iced_x86::Register;

/// Named guest registers exposed through the public API.
///
/// FS/GS bases are present from the start because real programs use them for
/// thread-local storage (§3.1).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Reg {
    Rax, Rbx, Rcx, Rdx, Rsi, Rdi, Rbp, Rsp,
    R8, R9, R10, R11, R12, R13, R14, R15,
    Rip,
    FsBase, GsBase,
}

impl Reg {
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
    // XMM/YMM (SIMD) land in a later milestone (§3.1, M8+):
    // pub xmm: [u128; 16],
}

impl CpuState {
    pub fn new() -> Self {
        Self::default()
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
