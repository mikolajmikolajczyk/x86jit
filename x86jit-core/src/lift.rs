//! Lift: x86 -> IR (§7).
//!
//! Two levels (§7.1): an operand-lowering layer beneath the per-mnemonic lift.
//! Every operand is reduced to a [`Val`] via [`lower_read`] / [`lower_write_target`]
//! before an op is emitted; memory operands expand to effective-address arithmetic
//! (the single [`effective_address`] helper, §17.5) plus `Load`/`Store`.

use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

use crate::ir::{Cond, FlagMask, IrBlock, IrOp, MemOrder, TempGen, Val, VLogicOp};
use crate::memory::Memory;
use crate::state::{iced_gpr_index, Reg};

/// Guest execution mode. Long mode only today; this is the seam (§17.3) that keeps
/// the literal `64` out of the decoder so a 32-bit mode could be added in one place.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CpuMode {
    Long64,
}

impl CpuMode {
    pub fn bits(self) -> u32 {
        match self {
            CpuMode::Long64 => 64,
        }
    }
}

/// A destination a result can be written to (§7.1).
pub enum WriteTarget {
    Reg { reg: Reg, size: u8 },
    Mem { addr: Val, size: u8 },
    /// A high-byte register (AH/BH/CH/DH — bits 8–15 of a GPR). Written by a
    /// read-mask-merge sequence on the parent; not expressible as a `Reg`.
    HighByte { parent: Reg },
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
/// The block ends at the first control-flow instruction (per iced's flow-control
/// classification, not a hand list) or when the mapped code runs out. `TempGen`
/// grows across the whole block. Emits `IrOp::InsnStart` at each instruction
/// boundary so a mid-block trap can set RIP to the faulting instruction (§8, §16).
pub fn lift_block(mem: &Memory, start: u64) -> Result<IrBlock, LiftError> {
    let mode = CpuMode::Long64;
    let code = mem
        .code_slice(start, 4096)
        .map_err(|_| LiftError::DecodeFault { addr: start })?;
    let mut decoder = Decoder::with_ip(mode.bits(), code, start, DecoderOptions::NONE);

    let mut ops = Vec::new();
    let mut tg = TempGen::new();
    let mut icount = 0u32;
    let mut guest_len = 0u32;
    let mut insn = Instruction::default();

    while decoder.can_decode() {
        decoder.decode_out(&mut insn);
        if insn.is_invalid() {
            return Err(LiftError::DecodeFault { addr: insn.ip() });
        }
        icount += 1;
        guest_len += insn.len() as u32;
        ops.push(IrOp::InsnStart {
            guest_addr: insn.ip(),
        });

        let terminated = lift_insn(&insn, code, start, &mut ops, &mut tg)?;
        if terminated {
            break;
        }
    }

    Ok(IrBlock {
        guest_start: start,
        ops,
        temp_count: tg.count(),
        guest_len,
        icount,
    })
}

/// Lift one instruction; returns `true` if it ends the block (control flow).
fn lift_insn(
    insn: &Instruction,
    code: &[u8],
    block_start: u64,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<bool, LiftError> {
    use Mnemonic::*;
    match insn.mnemonic() {
        Nop => Ok(false),

        Mov => {
            let src = lower_read(insn, 1, ops, tg)?;
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, src);
            Ok(false)
        }
        Lea => {
            // Address arithmetic only — no Load. Segment base is irrelevant to lea,
            // but effective_address is the single address path; lea operands carry
            // no segment prefix in practice.
            let addr = effective_address(insn, ops, tg)?;
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, addr);
            Ok(false)
        }

        Add => lift_binop(insn, ops, tg, BinOp::Add, FlagMask::ALL, true).map(|_| false),
        Adc => lift_binop(insn, ops, tg, BinOp::Adc, FlagMask::ALL, true).map(|_| false),
        Sub => lift_binop(insn, ops, tg, BinOp::Sub, FlagMask::ALL, true).map(|_| false),
        Sbb => lift_binop(insn, ops, tg, BinOp::Sbb, FlagMask::ALL, true).map(|_| false),
        And => lift_binop(insn, ops, tg, BinOp::And, FlagMask::ALL, true).map(|_| false),
        Or => lift_binop(insn, ops, tg, BinOp::Or, FlagMask::ALL, true).map(|_| false),
        Xor => lift_binop(insn, ops, tg, BinOp::Xor, FlagMask::ALL, true).map(|_| false),
        // cmp / test set flags but discard the result (no write-back).
        Cmp => lift_binop(insn, ops, tg, BinOp::Sub, FlagMask::ALL, false).map(|_| false),
        Test => lift_binop(insn, ops, tg, BinOp::And, FlagMask::ALL, false).map(|_| false),

        // shifts: count-conditional flags (§16), AF undefined → FlagMask::SHIFT.
        Shl => lift_binop(insn, ops, tg, BinOp::Shl, FlagMask::SHIFT, true).map(|_| false),
        Shr => lift_binop(insn, ops, tg, BinOp::Shr, FlagMask::SHIFT, true).map(|_| false),
        Sar => lift_binop(insn, ops, tg, BinOp::Sar, FlagMask::SHIFT, true).map(|_| false),
        // rotates: only CF/OF, count-conditional (CF_OF mask).
        Rol => lift_binop(insn, ops, tg, BinOp::Rol, FlagMask::CF_OF, true).map(|_| false),
        Ror => lift_binop(insn, ops, tg, BinOp::Ror, FlagMask::CF_OF, true).map(|_| false),

        // inc/dec keep CF (ALL_BUT_CF); neg is 0 - operand; not is bitwise, no flags.
        Inc => lift_incdec(insn, ops, tg, BinOp::Add).map(|_| false),
        Dec => lift_incdec(insn, ops, tg, BinOp::Sub).map(|_| false),
        Neg => lift_neg(insn, ops, tg).map(|_| false),
        Not => lift_not(insn, ops, tg).map(|_| false),

        Mul => lift_widening_mul(insn, ops, tg, false).map(|_| false),
        Imul => lift_imul(insn, ops, tg).map(|_| false),
        Div => lift_div(insn, ops, tg, false).map(|_| false),
        Idiv => lift_div(insn, ops, tg, true).map(|_| false),

        Bswap => lift_bswap(insn, ops, tg).map(|_| false),
        Xchg => lift_xchg(insn, ops, tg).map(|_| false),

        // --- SSE data movement + logic (§3.1 M8) ---
        Movdqa | Movdqu | Movaps | Movups => lift_vmov(insn, ops, tg, 16).map(|_| false),
        Movq => lift_vmov(insn, ops, tg, 8).map(|_| false),
        Movd => lift_vmov(insn, ops, tg, 4).map(|_| false),
        Pxor | Xorps => lift_vlogic(insn, ops, tg, VLogicOp::Xor).map(|_| false),
        Pand | Andps => lift_vlogic(insn, ops, tg, VLogicOp::And).map(|_| false),
        Por | Orps => lift_vlogic(insn, ops, tg, VLogicOp::Or).map(|_| false),
        Pandn | Andnps => lift_vlogic(insn, ops, tg, VLogicOp::Andn).map(|_| false),

        Movzx => lift_movzx(insn, ops, tg).map(|_| false),
        Movsx | Movsxd => lift_movsx(insn, ops, tg).map(|_| false),
        Cdqe => lift_cdqe(ops, tg).map(|_| false),
        Cqo => lift_cqo(ops, tg).map(|_| false),

        Push => lift_push(insn, ops, tg).map(|_| false),
        Pop => lift_pop(insn, ops, tg).map(|_| false),

        // --- control flow: ends the block ---
        Jmp => {
            let target = branch_target(insn, ops, tg)?;
            ops.push(IrOp::Jump { target });
            Ok(true)
        }
        Call => {
            let target = branch_target(insn, ops, tg)?;
            ops.push(IrOp::Call {
                target,
                return_addr: insn.next_ip(),
            });
            Ok(true)
        }
        Ret => {
            ops.push(IrOp::Ret);
            Ok(true)
        }
        Syscall => {
            ops.push(IrOp::Syscall);
            Ok(true)
        }
        Hlt => {
            ops.push(IrOp::Hlt);
            Ok(true)
        }

        _ => {
            if let Some(cond) = jcc_cond(insn.mnemonic()) {
                ops.push(IrOp::Branch {
                    cond,
                    taken: insn.near_branch_target(),
                    fallthrough: insn.next_ip(),
                });
                return Ok(true);
            }
            if let Some(cond) = setcc_cond(insn.mnemonic()) {
                return lift_setcc(insn, ops, tg, cond).map(|_| false);
            }
            if let Some(cond) = cmovcc_cond(insn.mnemonic()) {
                return lift_cmovcc(insn, ops, tg, cond).map(|_| false);
            }
            Err(unsupported(insn, code, block_start))
        }
    }
}

// --- per-mnemonic helpers ---

#[derive(Copy, Clone)]
enum BinOp {
    Add,
    Adc,
    Sub,
    Sbb,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
}

fn mk_binop(op: BinOp, dst: u32, a: Val, b: Val, size: u8, set_flags: FlagMask) -> IrOp {
    match op {
        BinOp::Add => IrOp::Add { dst, a, b, size, set_flags },
        BinOp::Adc => IrOp::Adc { dst, a, b, size, set_flags },
        BinOp::Sub => IrOp::Sub { dst, a, b, size, set_flags },
        BinOp::Sbb => IrOp::Sbb { dst, a, b, size, set_flags },
        BinOp::And => IrOp::And { dst, a, b, size, set_flags },
        BinOp::Or => IrOp::Or { dst, a, b, size, set_flags },
        BinOp::Xor => IrOp::Xor { dst, a, b, size, set_flags },
        BinOp::Shl => IrOp::Shl { dst, a, b, size, set_flags },
        BinOp::Shr => IrOp::Shr { dst, a, b, size, set_flags },
        BinOp::Sar => IrOp::Sar { dst, a, b, size, set_flags },
        BinOp::Rol => IrOp::Rol { dst, a, b, size, set_flags },
        BinOp::Ror => IrOp::Ror { dst, a, b, size, set_flags },
    }
}

/// Two-operand ALU lift. Handles the register/immediate/memory destination and the
/// read-modify-write case: for a memory destination the effective address is
/// computed ONCE (§7.1) and reused for Load and Store, with the Store emitted
/// before nothing else commits (atomicity, §16 pitfall #0 — flag recompute on
/// retry is idempotent from the same inputs).
fn lift_binop(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: BinOp,
    flags: FlagMask,
    write_back: bool,
) -> Result<(), LiftError> {
    let size = operation_size(insn);

    if insn.op_kind(0) == OpKind::Memory {
        // dst is memory: compute the address once.
        let addr = effective_address(insn, ops, tg)?;
        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        let b = lower_read(insn, 1, ops, tg)?;
        let res = tg.fresh();
        ops.push(mk_binop(op, res, a, b, size, flags));
        if write_back {
            ops.push(IrOp::Store {
                addr,
                src: Val::Temp(res),
                size,
                order: MemOrder::None,
            });
        }
        return Ok(());
    }

    let a = lower_read(insn, 0, ops, tg)?;
    let b = lower_read(insn, 1, ops, tg)?;
    let res = tg.fresh();
    ops.push(mk_binop(op, res, a, b, size, flags));
    if write_back {
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(res));
    }
    Ok(())
}

/// `push src` — long-mode default operand size is 8. Store BEFORE committing RSP so
/// a faulting store leaves RSP untouched for the retry (§16 pitfall #0).
fn lift_push(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = push_pop_size(insn);
    let src = lower_read(insn, 0, ops, tg)?;

    let rsp = read_reg(Reg::Rsp, ops, tg);
    let new_rsp = tg.fresh();
    ops.push(IrOp::Sub {
        dst: new_rsp,
        a: rsp,
        b: Val::Imm(size as u64),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    ops.push(IrOp::Store {
        addr: Val::Temp(new_rsp),
        src,
        size,
        order: MemOrder::None,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: 8,
    });
    Ok(())
}

/// `pop dst` — Load BEFORE committing so a faulting load leaves state untouched.
/// `pop rsp` works because the destination write is emitted last and overrides the
/// RSP increment.
fn lift_pop(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = push_pop_size(insn);
    let rsp = read_reg(Reg::Rsp, ops, tg);
    let val = tg.fresh();
    ops.push(IrOp::Load {
        dst: val,
        addr: rsp,
        size,
    });
    let new_rsp = tg.fresh();
    ops.push(IrOp::Add {
        dst: new_rsp,
        a: rsp,
        b: Val::Imm(size as u64),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: 8,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(val));
    Ok(())
}

/// `inc`/`dec`: `op0 ± 1`, preserving CF (`ALL_BUT_CF`). RMW-safe via lift_binop's
/// memory path (the immediate 1 is the second source).
fn lift_incdec(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: BinOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        let res = tg.fresh();
        ops.push(mk_binop(op, res, a, Val::Imm(1), size, FlagMask::ALL_BUT_CF));
        ops.push(IrOp::Store { addr, src: Val::Temp(res), size, order: MemOrder::None });
        return Ok(());
    }
    let a = lower_read(insn, 0, ops, tg)?;
    let res = tg.fresh();
    ops.push(mk_binop(op, res, a, Val::Imm(1), size, FlagMask::ALL_BUT_CF));
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(res));
    Ok(())
}

/// `neg`: `0 - op0`. Flags exactly as `sub` from zero (CF set iff operand ≠ 0).
fn lift_neg(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        let res = tg.fresh();
        ops.push(IrOp::Sub { dst: res, a: Val::Imm(0), b: a, size, set_flags: FlagMask::ALL });
        ops.push(IrOp::Store { addr, src: Val::Temp(res), size, order: MemOrder::None });
        return Ok(());
    }
    let a = lower_read(insn, 0, ops, tg)?;
    let res = tg.fresh();
    ops.push(IrOp::Sub { dst: res, a: Val::Imm(0), b: a, size, set_flags: FlagMask::ALL });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(res));
    Ok(())
}

/// `not`: bitwise complement, NO flags. Lowered as `xor op0, -1` with an empty
/// flag mask (the result is masked to the operand size by the interpreter).
fn lift_not(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        let res = tg.fresh();
        ops.push(IrOp::Xor { dst: res, a, b: Val::Imm(u64::MAX), size, set_flags: FlagMask::NONE });
        ops.push(IrOp::Store { addr, src: Val::Temp(res), size, order: MemOrder::None });
        return Ok(());
    }
    let a = lower_read(insn, 0, ops, tg)?;
    let res = tg.fresh();
    ops.push(IrOp::Xor { dst: res, a, b: Val::Imm(u64::MAX), size, set_flags: FlagMask::NONE });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(res));
    Ok(())
}

/// One-operand `mul`/`imul`: `RDX:RAX = RAX * op0`. 8-bit form writes AH (not
/// expressible), so it's rejected; 16/32/64-bit split into RAX (low) and RDX (high).
fn lift_widening_mul(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    signed: bool,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if size < 2 {
        return Err(unsupported_insn(insn));
    }
    let a = read_reg(Reg::Rax, ops, tg);
    let b = lower_read(insn, 0, ops, tg)?;
    let lo = tg.fresh();
    let hi = tg.fresh();
    ops.push(IrOp::Mul { lo, hi, a, b, size, signed, set_flags: FlagMask::CF_OF });
    ops.push(IrOp::WriteReg { reg: Reg::Rax, src: Val::Temp(lo), size });
    ops.push(IrOp::WriteReg { reg: Reg::Rdx, src: Val::Temp(hi), size });
    Ok(())
}

/// `imul`: one-operand (`RDX:RAX`), two-operand (`dst *= src`), or three-operand
/// (`dst = src * imm`). The 2/3-operand forms keep only the low half in `dst`;
/// CF/OF still flag overflow of the full signed product.
fn lift_imul(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    match insn.op_count() {
        1 => lift_widening_mul(insn, ops, tg, true),
        2 | 3 => {
            let size = operand_size(insn, 0);
            let (a, b) = if insn.op_count() == 2 {
                (lower_read(insn, 0, ops, tg)?, lower_read(insn, 1, ops, tg)?)
            } else {
                (lower_read(insn, 1, ops, tg)?, lower_read(insn, 2, ops, tg)?)
            };
            let lo = tg.fresh();
            let hi = tg.fresh();
            ops.push(IrOp::Mul { lo, hi, a, b, size, signed: true, set_flags: FlagMask::CF_OF });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(lo));
            Ok(())
        }
        _ => Err(unsupported_insn(insn)),
    }
}

/// `div`/`idiv`: `RDX:RAX / op0` → RAX quotient, RDX remainder. May raise `#DE`
/// (zero divisor / overflow) — the `Div` op traps before the register writes, so a
/// retry sees clean state (§16). 8-bit form writes AH, so it's rejected.
fn lift_div(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    signed: bool,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if size < 2 {
        return Err(unsupported_insn(insn));
    }
    let hi = read_reg(Reg::Rdx, ops, tg);
    let lo = read_reg(Reg::Rax, ops, tg);
    let divisor = lower_read(insn, 0, ops, tg)?;
    let quot = tg.fresh();
    let rem = tg.fresh();
    ops.push(IrOp::Div { quot, rem, hi, lo, divisor, size, signed });
    ops.push(IrOp::WriteReg { reg: Reg::Rax, src: Val::Temp(quot), size });
    ops.push(IrOp::WriteReg { reg: Reg::Rdx, src: Val::Temp(rem), size });
    Ok(())
}

/// XMM register index (0–15) for an operand, or `None` if it isn't an XMM reg.
fn reg_xmm(insn: &Instruction, op_idx: u32) -> Option<u8> {
    if insn.op_kind(op_idx) != OpKind::Register {
        return None;
    }
    let r = insn.op_register(op_idx);
    r.is_xmm().then(|| (r as u32 - Register::XMM0 as u32) as u8)
}

/// SSE move (movdqa/movdqu/movaps/movups = 16, movq = 8, movd = 4) between
/// xmm/gpr/memory. `movq`/`movd` reg forms move the low `size` bytes and zero the
/// upper part of the destination xmm.
fn lift_vmov(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    size: u8,
) -> Result<(), LiftError> {
    let x0 = reg_xmm(insn, 0);
    let x1 = reg_xmm(insn, 1);
    let (k0, k1) = (insn.op_kind(0), insn.op_kind(1));

    if let Some(d) = x0 {
        if k1 == OpKind::Memory {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VLoad { dst: d, addr, size });
            return Ok(());
        }
        if let Some(s) = x1 {
            if size == 16 {
                ops.push(IrOp::VMov { dst: d, src: s });
            } else {
                // low bytes only, upper zeroed — round-trip through a GPR temp.
                let t = tg.fresh();
                ops.push(IrOp::VToGpr { dst: t, src: s, size });
                ops.push(IrOp::VFromGpr { dst: d, src: Val::Temp(t), size });
            }
            return Ok(());
        }
        if k1 == OpKind::Register {
            let g = lower_read(insn, 1, ops, tg)?;
            ops.push(IrOp::VFromGpr { dst: d, src: g, size });
            return Ok(());
        }
    }
    if let Some(s) = x1 {
        if k0 == OpKind::Memory {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VStore { addr, src: s, size });
            return Ok(());
        }
        if k0 == OpKind::Register {
            let t = tg.fresh();
            ops.push(IrOp::VToGpr { dst: t, src: s, size });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(t));
            return Ok(());
        }
    }
    Err(unsupported_insn(insn))
}

/// SSE bitwise logic (pxor/pand/por/pandn + *ps aliases). Register source only
/// for now (memory source deferred). `dst = op(dst, src)`.
fn lift_vlogic(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    _tg: &mut TempGen,
    op: VLogicOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VLogic { dst: d, a: d, b, op });
    Ok(())
}

/// `bswap`: reverse the byte order of a 32/64-bit register. No flags.
fn lift_bswap(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let a = lower_read(insn, 0, ops, tg)?;
    let t = tg.fresh();
    ops.push(IrOp::Bswap { dst: t, a, size });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `xchg dst, src`: swap the two operands. Both values are read before either
/// write (and both write targets computed first) so the swap is atomic w.r.t. this
/// instruction. Locking (for a memory operand) is a no-op single-threaded.
fn lift_xchg(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let a_val = lower_read(insn, 0, ops, tg)?;
    let b_val = lower_read(insn, 1, ops, tg)?;
    let dst0 = lower_write_target(insn, 0, ops, tg)?;
    let dst1 = lower_write_target(insn, 1, ops, tg)?;
    emit_write(ops, tg, dst0, b_val);
    emit_write(ops, tg, dst1, a_val);
    Ok(())
}

/// `movzx`: zero-extend the source (mask to its width), write with the dst width.
fn lift_movzx(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let src_size = operand_size(insn, 1);
    let v = lower_read(insn, 1, ops, tg)?;
    let z = tg.fresh();
    ops.push(IrOp::And {
        dst: z,
        a: v,
        b: Val::Imm(size_mask(src_size)),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(z));
    Ok(())
}

/// `movsx`/`movsxd`: sign-extend the source to 64 bits, write with the dst width.
fn lift_movsx(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let src_size = operand_size(insn, 1);
    let v = lower_read(insn, 1, ops, tg)?;
    let s = tg.fresh();
    ops.push(IrOp::Sext { dst: s, a: v, from: src_size });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(s));
    Ok(())
}

/// `cdqe`: sign-extend EAX into RAX.
fn lift_cdqe(ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let rax = read_reg(Reg::Rax, ops, tg);
    let s = tg.fresh();
    ops.push(IrOp::Sext { dst: s, a: rax, from: 4 });
    ops.push(IrOp::WriteReg { reg: Reg::Rax, src: Val::Temp(s), size: 8 });
    Ok(())
}

/// `cqo`: RDX = sign of RAX (arithmetic shift by 63 → all-zero or all-one).
fn lift_cqo(ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let rax = read_reg(Reg::Rax, ops, tg);
    let s = tg.fresh();
    ops.push(IrOp::Sar { dst: s, a: rax, b: Val::Imm(63), size: 8, set_flags: FlagMask::NONE });
    ops.push(IrOp::WriteReg { reg: Reg::Rdx, src: Val::Temp(s), size: 8 });
    Ok(())
}

/// `setcc r/m8`: materialize the condition as 0/1 into an 8-bit destination.
fn lift_setcc(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    cond: Cond,
) -> Result<(), LiftError> {
    let c = tg.fresh();
    ops.push(IrOp::GetCond { dst: c, cond });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(c));
    Ok(())
}

/// `cmovcc dst, src`: branchless conditional move. cmov ALWAYS writes the register
/// (so a 32-bit cmov zero-extends even when not taken); the select is
/// `dst ^ ((dst ^ src) & mask)` where `mask` is all-ones iff the condition holds.
fn lift_cmovcc(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    cond: Cond,
) -> Result<(), LiftError> {
    let src = lower_read(insn, 1, ops, tg)?;
    let dst_val = lower_read(insn, 0, ops, tg)?;

    let c = tg.fresh();
    ops.push(IrOp::GetCond { dst: c, cond });
    // mask = 0 - c  → 0x0 or 0xFFFF...FF
    let m = tg.fresh();
    ops.push(IrOp::Sub {
        dst: m,
        a: Val::Imm(0),
        b: Val::Temp(c),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    // diff = dst ^ src
    let diff = tg.fresh();
    ops.push(IrOp::Xor { dst: diff, a: dst_val, b: src, size: 8, set_flags: FlagMask::NONE });
    // sel = diff & mask
    let sel = tg.fresh();
    ops.push(IrOp::And {
        dst: sel,
        a: Val::Temp(diff),
        b: Val::Temp(m),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    // res = dst ^ sel
    let res = tg.fresh();
    ops.push(IrOp::Xor {
        dst: res,
        a: dst_val,
        b: Val::Temp(sel),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(res));
    Ok(())
}

/// Mask covering the low `size` bytes.
fn size_mask(size: u8) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

// --- operand lowering (§7.1) ---

/// Reduce a SOURCE operand to a `Val` (reads reg / immediate / loads memory).
fn lower_read(
    insn: &Instruction,
    op_idx: u32,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<Val, LiftError> {
    match insn.op_kind(op_idx) {
        OpKind::Register => {
            let r = insn.op_register(op_idx);
            if let Some(parent) = high_byte_parent(r) {
                // Read AH/BH/CH/DH = (parent >> 8) & 0xff.
                let p = read_reg(parent, ops, tg);
                let sh = alu_none(ops, tg, |dst| IrOp::Shr {
                    dst,
                    a: p,
                    b: Val::Imm(8),
                    size: 8,
                    set_flags: FlagMask::NONE,
                });
                return Ok(alu_none(ops, tg, |dst| IrOp::And {
                    dst,
                    a: sh,
                    b: Val::Imm(0xff),
                    size: 8,
                    set_flags: FlagMask::NONE,
                }));
            }
            let reg = iced_to_reg(r).ok_or_else(|| unsupported_insn(insn))?;
            Ok(read_reg(reg, ops, tg))
        }
        OpKind::Memory => {
            let addr = effective_address(insn, ops, tg)?;
            let size = operand_size(insn, op_idx);
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Ok(Val::Temp(t))
        }
        kind if is_immediate(kind) => Ok(Val::Imm(insn.immediate(op_idx))),
        _ => Err(unsupported_insn(insn)),
    }
}

/// Reduce a DESTINATION operand to a write handle (reg or memory address).
fn lower_write_target(
    insn: &Instruction,
    op_idx: u32,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<WriteTarget, LiftError> {
    match insn.op_kind(op_idx) {
        OpKind::Register => {
            let r = insn.op_register(op_idx);
            if let Some(parent) = high_byte_parent(r) {
                return Ok(WriteTarget::HighByte { parent });
            }
            let reg = iced_to_reg(r).ok_or_else(|| unsupported_insn(insn))?;
            Ok(WriteTarget::Reg {
                reg,
                size: operand_size(insn, op_idx),
            })
        }
        OpKind::Memory => {
            let addr = effective_address(insn, ops, tg)?;
            Ok(WriteTarget::Mem {
                addr,
                size: operand_size(insn, op_idx),
            })
        }
        _ => Err(unsupported_insn(insn)),
    }
}

/// The GPR whose bits 8–15 a high-byte register names, or `None`.
fn high_byte_parent(reg: Register) -> Option<Reg> {
    match reg {
        Register::AH => Some(Reg::Rax),
        Register::BH => Some(Reg::Rbx),
        Register::CH => Some(Reg::Rcx),
        Register::DH => Some(Reg::Rdx),
        _ => None,
    }
}

/// Emit `base + index*scale + disp`, returning a `Val` holding the address.
/// The ONE place an address is computed (§17.5). Uses iced's folded RIP-relative
/// value (next-insn base) and adds FS/GS base when a segment prefix is present.
fn effective_address(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<Val, LiftError> {
    let base = insn.memory_base();
    let index = insn.memory_index();
    let scale = insn.memory_index_scale();
    let disp = insn.memory_displacement64();

    // RIP-relative: iced already folded RIP+disp into an absolute address.
    if base == Register::RIP || base == Register::EIP {
        return Ok(with_segment(insn, Val::Imm(disp), ops, tg));
    }

    let mut acc: Option<Val> = None;

    if base != Register::None {
        let reg = iced_to_reg(base).ok_or_else(|| unsupported_insn(insn))?;
        acc = Some(read_reg(reg, ops, tg));
    }

    if index != Register::None {
        let reg = iced_to_reg(index).ok_or_else(|| unsupported_insn(insn))?;
        let idx = read_reg(reg, ops, tg);
        let scaled = if scale <= 1 {
            idx
        } else {
            // scale ∈ {2,4,8} → shift by {1,2,3}. No flags.
            let shift = scale.trailing_zeros() as u64;
            let t = tg.fresh();
            ops.push(IrOp::Shl {
                dst: t,
                a: idx,
                b: Val::Imm(shift),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            Val::Temp(t)
        };
        acc = Some(match acc {
            None => scaled,
            Some(a) => add_addr(a, scaled, ops, tg),
        });
    }

    let addr = match acc {
        None => Val::Imm(disp),
        Some(a) if disp == 0 => a,
        Some(a) => add_addr(a, Val::Imm(disp), ops, tg),
    };

    Ok(with_segment(insn, addr, ops, tg))
}

/// Add the FS/GS segment base if the instruction carries that prefix (TLS, §7.1).
fn with_segment(insn: &Instruction, addr: Val, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val {
    let seg = match insn.segment_prefix() {
        Register::FS => Reg::FsBase,
        Register::GS => Reg::GsBase,
        _ => return addr,
    };
    let base = read_reg(seg, ops, tg);
    add_addr(addr, base, ops, tg)
}

/// Emit a non-flag-setting 64-bit address addition.
fn add_addr(a: Val, b: Val, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val {
    let t = tg.fresh();
    ops.push(IrOp::Add {
        dst: t,
        a,
        b,
        size: 8,
        set_flags: FlagMask::NONE,
    });
    Val::Temp(t)
}

/// Emit a `ReadReg` and return the temp holding the value.
fn read_reg(reg: Reg, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val {
    let t = tg.fresh();
    ops.push(IrOp::ReadReg { dst: t, reg });
    Val::Temp(t)
}

fn emit_write(ops: &mut Vec<IrOp>, tg: &mut TempGen, target: WriteTarget, value: Val) {
    match target {
        WriteTarget::Reg { reg, size } => ops.push(IrOp::WriteReg {
            reg,
            src: value,
            size,
        }),
        WriteTarget::Mem { addr, size } => ops.push(IrOp::Store {
            addr,
            src: value,
            size,
            order: MemOrder::None,
        }),
        // AH/BH/CH/DH: parent = (parent & ~0xff00) | ((value & 0xff) << 8).
        WriteTarget::HighByte { parent } => {
            let cur = read_reg(parent, ops, tg);
            let clear = alu_none(ops, tg, |dst| IrOp::And {
                dst,
                a: cur,
                b: Val::Imm(!0xff00u64),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            let byte = alu_none(ops, tg, |dst| IrOp::And {
                dst,
                a: value,
                b: Val::Imm(0xff),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            let shifted = alu_none(ops, tg, |dst| IrOp::Shl {
                dst,
                a: byte,
                b: Val::Imm(8),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            let merged = alu_none(ops, tg, |dst| IrOp::Or {
                dst,
                a: clear,
                b: shifted,
                size: 8,
                set_flags: FlagMask::NONE,
            });
            ops.push(IrOp::WriteReg { reg: parent, src: merged, size: 8 });
        }
    }
}

/// Emit a flag-free op producing a fresh temp, returning it as a `Val`.
fn alu_none(ops: &mut Vec<IrOp>, tg: &mut TempGen, mk: impl FnOnce(u32) -> IrOp) -> Val {
    let t = tg.fresh();
    ops.push(mk(t));
    Val::Temp(t)
}

/// Target `Val` for a jmp/call: an immediate for a near (rel) branch, otherwise the
/// value of the indirect register/memory operand.
fn branch_target(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<Val, LiftError> {
    match insn.op_kind(0) {
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
            Ok(Val::Imm(insn.near_branch_target()))
        }
        _ => lower_read(insn, 0, ops, tg),
    }
}

// --- small helpers ---

/// Map an iced register (any width GPR) to our `Reg`. `None` for high-byte
/// registers (AH/BH/CH/DH, which write bits 8–15 and can't be expressed by the
/// 64-bit `Reg` enum) and non-GPRs — the caller turns `None` into `Unsupported`
/// rather than mis-lowering to the low byte.
fn iced_to_reg(reg: Register) -> Option<Reg> {
    if matches!(reg, Register::AH | Register::BH | Register::CH | Register::DH) {
        return None;
    }
    iced_gpr_index(reg).map(Reg::from_gpr_index)
}

fn is_immediate(kind: OpKind) -> bool {
    matches!(
        kind,
        OpKind::Immediate8
            | OpKind::Immediate8_2nd
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate64
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64
    )
}

/// Size in bytes of one operand (register width or memory-access width).
fn operand_size(insn: &Instruction, op_idx: u32) -> u8 {
    match insn.op_kind(op_idx) {
        OpKind::Register => insn.op_register(op_idx).size() as u8,
        OpKind::Memory => insn.memory_size().size() as u8,
        _ => 0,
    }
}

/// Width of a binary operation = size of operand 0 (the destination), falling back
/// to operand 1 for the rare all-immediate/implicit form.
fn operation_size(insn: &Instruction) -> u8 {
    let s = operand_size(insn, 0);
    if s != 0 {
        s
    } else {
        operand_size(insn, 1)
    }
}

/// push/pop transfer size — long-mode default is 8; a 16-bit operand overrides.
fn push_pop_size(insn: &Instruction) -> u8 {
    let s = operand_size(insn, 0);
    if s == 0 {
        8
    } else {
        s
    }
}

fn jcc_cond(m: Mnemonic) -> Option<Cond> {
    use Mnemonic::*;
    Some(match m {
        Je => Cond::Eq,
        Jne => Cond::Ne,
        Jb => Cond::Below,
        Jae => Cond::AboveEq,
        Jbe => Cond::BelowEq,
        Ja => Cond::Above,
        Jl => Cond::Less,
        Jge => Cond::GreaterEq,
        Jle => Cond::LessEq,
        Jg => Cond::Greater,
        Js => Cond::Sign,
        Jns => Cond::NoSign,
        Jo => Cond::Overflow,
        Jno => Cond::NoOverflow,
        Jp => Cond::Parity,
        Jnp => Cond::NoParity,
        _ => return None,
    })
}

fn setcc_cond(m: Mnemonic) -> Option<Cond> {
    use Mnemonic::*;
    Some(match m {
        Sete => Cond::Eq,
        Setne => Cond::Ne,
        Setb => Cond::Below,
        Setae => Cond::AboveEq,
        Setbe => Cond::BelowEq,
        Seta => Cond::Above,
        Setl => Cond::Less,
        Setge => Cond::GreaterEq,
        Setle => Cond::LessEq,
        Setg => Cond::Greater,
        Sets => Cond::Sign,
        Setns => Cond::NoSign,
        Seto => Cond::Overflow,
        Setno => Cond::NoOverflow,
        Setp => Cond::Parity,
        Setnp => Cond::NoParity,
        _ => return None,
    })
}

fn cmovcc_cond(m: Mnemonic) -> Option<Cond> {
    use Mnemonic::*;
    Some(match m {
        Cmove => Cond::Eq,
        Cmovne => Cond::Ne,
        Cmovb => Cond::Below,
        Cmovae => Cond::AboveEq,
        Cmovbe => Cond::BelowEq,
        Cmova => Cond::Above,
        Cmovl => Cond::Less,
        Cmovge => Cond::GreaterEq,
        Cmovle => Cond::LessEq,
        Cmovg => Cond::Greater,
        Cmovs => Cond::Sign,
        Cmovns => Cond::NoSign,
        Cmovo => Cond::Overflow,
        Cmovno => Cond::NoOverflow,
        Cmovp => Cond::Parity,
        Cmovnp => Cond::NoParity,
        _ => return None,
    })
}

fn unsupported_insn(insn: &Instruction) -> LiftError {
    LiftError::Unsupported {
        addr: insn.ip(),
        bytes: [0; 15],
        len: insn.len() as u8,
    }
}

fn unsupported(insn: &Instruction, code: &[u8], block_start: u64) -> LiftError {
    let mut bytes = [0u8; 15];
    let off = (insn.ip() - block_start) as usize;
    let len = insn.len();
    if let Some(slice) = code.get(off..off + len) {
        bytes[..len].copy_from_slice(slice);
    }
    LiftError::Unsupported {
        addr: insn.ip(),
        bytes,
        len: len as u8,
    }
}
