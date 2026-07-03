//! Lift: x86 -> IR (§7).
//!
//! The lift is a `match` over the iced-x86 mnemonic. Crucially it has an
//! operand-lowering layer *beneath* the per-mnemonic lift (§7.1): every operand
//! is reduced to a [`Val`] before an op is emitted, and memory operands expand
//! to effective-address arithmetic + `Load`/`Store`.

use crate::ir::{IrBlock, IrOp, TempGen, Val};
use crate::state::Reg;

/// A destination a result can be written to (§7.1).
pub enum WriteTarget {
    Reg(Reg),
    Mem { addr: Val, size: u8 },
}

/// Lift errors are mapped to `Exit` in the dispatcher, never to a panic (§7.3).
#[derive(Debug)]
pub enum LiftError {
    /// Decoded by iced, but the lift does not handle it yet.
    Unsupported { addr: u64, bytes: [u8; 15], len: u8 },
    /// Could not even decode (garbage / bytes outside mapped memory).
    DecodeFault { addr: u64 },
}

/// Lift a single basic block starting at guest address `start` (§7.3).
///
/// The block ends at the first control-flow instruction. `TempGen` grows across
/// the whole block and is not reset between instructions.
///
/// Decodes from `mem.code_slice(start, ..)` (iced needs a byte slice, not scalar
/// reads). Emits `IrOp::InsnStart { guest_addr }` at each instruction boundary so
/// a mem-trap can set RIP to the faulting instruction (§6.2, §8). Decoder bitness
/// comes from `CpuMode` (SEAM §17.3) — today always Long64; thread it in when the
/// lift context grows.
pub fn lift_block(_mem: &crate::memory::Memory, _start: u64) -> Result<IrBlock, LiftError> {
    todo!("M1: code_slice -> iced Decoder loop (InsnStart per insn) -> per-mnemonic lift -> IrBlock")
}

// --- operand lowering helpers (§7.1) — used by every per-mnemonic lift ---

/// Reduce a SOURCE operand to a `Val` (reads reg / immediate / loads memory).
fn _lower_read(_op_idx: u32, _ops: &mut Vec<IrOp>, _tg: &mut TempGen) -> Val {
    todo!("§7.1")
}

/// Reduce a DESTINATION operand to a write handle (reg or memory address).
fn _lower_write_target(_op_idx: u32, _ops: &mut Vec<IrOp>, _tg: &mut TempGen) -> WriteTarget {
    todo!("§7.1")
}

/// Emit `base + index*scale + disp`, returning a `Val` holding the address.
/// Handles RIP-relative (use iced's computed value) and FS/GS segment bases.
fn _effective_address(_ops: &mut Vec<IrOp>, _tg: &mut TempGen) -> Val {
    todo!("§7.1 effective address")
}
