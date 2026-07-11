use super::*;

/// `in`/`out` (imm8 or `dx` form) → `IrOp::PortIo`, a trap-out to the embedder
/// (§5.2). Operand layout (iced): `in acc, port` has op0 = accumulator (`al`/`ax`/
/// `eax`), op1 = the port (imm8 or `dx`); `out port, acc` is the mirror. The access
/// width is the accumulator's operand size (1/2/4). For `out` the accumulator value
/// is read here and carried in the exit; for `in` the embedder writes the result
/// back via `complete_port_in`.
pub(crate) fn lift_port_io(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    dir_out: bool,
) -> Result<(), LiftError> {
    let (acc_idx, port_idx) = if dir_out { (1, 0) } else { (0, 1) };
    let size = operand_size(insn, acc_idx);
    if !matches!(size, 1 | 2 | 4) {
        return Err(unsupported_insn(insn));
    }

    // Port is either an 8-bit immediate or `dx` (low 16 bits).
    let port = match insn.op_kind(port_idx) {
        OpKind::Immediate8 => Val::Imm(insn.immediate(port_idx) & 0xffff),
        OpKind::Register => {
            let dx = read_reg(Reg::Rdx, ops, tg);
            alu_none(ops, tg, |dst| IrOp::And {
                dst,
                a: dx,
                b: Val::Imm(0xffff),
                size: 8,
                set_flags: FlagMask::NONE,
            })
        }
        _ => return Err(unsupported_insn(insn)),
    };

    let value = if dir_out {
        read_reg(Reg::Rax, ops, tg)
    } else {
        Val::Imm(0)
    };

    ops.push(IrOp::PortIo {
        port,
        value,
        size,
        dir_out,
    });
    Ok(())
}

/// `push src` — long-mode default operand size is 8. Store BEFORE committing RSP so
/// a faulting store leaves RSP untouched for the retry (§16 pitfall #0).
pub(crate) fn lift_push(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mode: CpuMode,
) -> Result<(), LiftError> {
    let size = push_pop_size(insn, mode);
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
    // Compat32: ESP wraps mod 2^32 before it is used as the store address (a push at
    // ESP < slot must wrap, not carry into the upper half of the backing u64) (§16).
    emit_sp_wrap(ops, new_rsp, mode);
    ops.push(IrOp::Store {
        addr: Val::Temp(new_rsp),
        src,
        size,
        order: MemOrder::None,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: sp_write_size(mode),
    });
    Ok(())
}

/// `pop dst` — Load BEFORE committing so a faulting load leaves state untouched.
/// `pop rsp` works because the destination write is emitted last and overrides the
/// RSP increment.
pub(crate) fn lift_pop(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mode: CpuMode,
) -> Result<(), LiftError> {
    let size = push_pop_size(insn, mode);
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
    // A 4-byte RSP write in Compat32 zero-extends → ESP wraps mod 2^32. `pop rsp`
    // works because the destination write is emitted last and overrides this.
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: sp_write_size(mode),
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(val));
    Ok(())
}

/// A string op with its repeat prefix. movs/stos/lods take `rep`; scas/cmps take
/// `repe`/`repne` (both share the F3/F2 prefix bytes with the instruction kind).
pub(crate) fn lift_string(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: StrOp,
    elem: u8,
) -> Result<bool, LiftError> {
    let f3 = insn.has_rep_prefix() || insn.has_repe_prefix();
    let f2 = insn.has_repne_prefix();
    let rep = match op {
        StrOp::Scas | StrOp::Cmps => {
            if f2 {
                RepKind::Repne
            } else if f3 {
                RepKind::Repe
            } else {
                RepKind::None
            }
        }
        _ => {
            if f3 {
                RepKind::Rep
            } else {
                RepKind::None
            }
        }
    };
    ops.push(IrOp::RepString { op, elem, rep });
    Ok(false)
}

/// The ST(i) index referenced by an x87 instruction: the highest ST register
/// among its operands (ST0 is index 0, so a non-zero partner wins). Defaults to 1
/// for the implicit-`st1` forms (`faddp`, `fxch`).
pub(crate) fn st_index(insn: &Instruction) -> u8 {
    let mut idx = None;
    for i in 0..insn.op_count() {
        let r = insn.op_register(i);
        if r >= Register::ST0 && r <= Register::ST7 {
            let n = (r as u32 - Register::ST0 as u32) as u8;
            idx = Some(idx.map_or(n, |c: u8| c.max(n)));
        }
    }
    idx.unwrap_or(1)
}

/// For a register-form x87 arithmetic/store instruction, is the destination ST(0)?
/// (`fsub st(0), st(i)` vs `fsub st(i), st(0)` — op0 is the destination.) Chooses
/// between the `*Sti` (ST0-dest) and `*ToSti` (ST(i)-dest) IR kinds.
pub(crate) fn dst_is_st0(insn: &Instruction) -> bool {
    insn.op0_register() == Register::ST0
}

/// Lift one x87 FPU instruction to an `X87` IR op (§14). Memory operands are
/// reduced to an effective address; register forms carry ST(i) in `sti`.
pub(crate) fn lift_x87(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    use crate::x87::FpuKind as K;
    use Mnemonic::*;

    let mem = (0..insn.op_count()).any(|i| insn.op_kind(i) == OpKind::Memory);
    let msz = insn.memory_size().size();
    let sti = st_index(insn);

    // Emit an X87 op with a freshly computed address (memory forms) or a dummy.
    let emit = |kind: K, ops: &mut Vec<IrOp>, tg: &mut TempGen| -> Result<(), LiftError> {
        let addr = if mem {
            effective_address(insn, ops, tg)?
        } else {
            Val::Imm(0)
        };
        ops.push(IrOp::X87 { kind, addr, sti });
        Ok(())
    };

    match insn.mnemonic() {
        Fld => {
            let k = if mem {
                match msz {
                    4 => K::FldF32,
                    10 => K::FldF80,
                    _ => K::FldF64,
                }
            } else {
                K::FldSti
            };
            emit(k, ops, tg)?;
        }
        Fild => {
            let k = match msz {
                2 => K::FildI16,
                8 => K::FildI64,
                _ => K::FildI32,
            };
            emit(k, ops, tg)?;
        }
        Fst => emit(
            if !mem {
                K::FstSti
            } else if msz == 4 {
                K::FstF32
            } else {
                K::FstF64
            },
            ops,
            tg,
        )?,
        Fstp => {
            let k = if !mem {
                K::FstpSti
            } else {
                match msz {
                    4 => K::FstpF32,
                    10 => K::FstpF80,
                    _ => K::FstpF64,
                }
            };
            emit(k, ops, tg)?;
        }
        Fistp => emit(
            match msz {
                2 => K::FistpI16,
                8 => K::FistpI64,
                _ => K::FistpI32,
            },
            ops,
            tg,
        )?,
        Fisttp => emit(
            match msz {
                2 => K::FisttpI16,
                8 => K::FisttpI64,
                _ => K::FisttpI32,
            },
            ops,
            tg,
        )?,
        Fadd => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FaddSti
                } else {
                    K::FaddToSti
                }
            } else if msz == 4 {
                K::FaddMemF32
            } else {
                K::FaddMemF64
            },
            ops,
            tg,
        )?,
        Faddp => emit(K::FaddP, ops, tg)?,
        Fsub => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FsubSti
                } else {
                    K::FsubToSti
                }
            } else if msz == 4 {
                K::FsubMemF32
            } else {
                K::FsubMemF64
            },
            ops,
            tg,
        )?,
        Fsubp => emit(K::FsubP, ops, tg)?,
        Fsubr => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FsubrSti
                } else {
                    K::FsubrToSti
                }
            } else if msz == 4 {
                K::FsubrMemF32
            } else {
                K::FsubrMemF64
            },
            ops,
            tg,
        )?,
        Fsubrp => emit(K::FsubrP, ops, tg)?,
        Fmul => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FmulSti
                } else {
                    K::FmulToSti
                }
            } else if msz == 4 {
                K::FmulMemF32
            } else {
                K::FmulMemF64
            },
            ops,
            tg,
        )?,
        Fmulp => emit(K::FmulP, ops, tg)?,
        Fdiv => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FdivSti
                } else {
                    K::FdivToSti
                }
            } else if msz == 4 {
                K::FdivMemF32
            } else {
                K::FdivMemF64
            },
            ops,
            tg,
        )?,
        Fdivp => emit(K::FdivP, ops, tg)?,
        Fdivr => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FdivrSti
                } else {
                    K::FdivrToSti
                }
            } else if msz == 4 {
                K::FdivrMemF32
            } else {
                K::FdivrMemF64
            },
            ops,
            tg,
        )?,
        Fdivrp => emit(K::FdivrP, ops, tg)?,
        Fld1 => emit(K::Fld1, ops, tg)?,
        Fldz => emit(K::Fldz, ops, tg)?,
        Fabs => emit(K::Fabs, ops, tg)?,
        Fchs => emit(K::Fchs, ops, tg)?,
        Fxch => emit(K::Fxch, ops, tg)?,
        Fucomi => emit(K::Fucomi, ops, tg)?,
        Fucomip => emit(K::Fucomip, ops, tg)?,
        Fcomi => emit(K::Fcomi, ops, tg)?,
        Fcomip => emit(K::Fcomip, ops, tg)?,
        Fldcw => emit(K::Fldcw, ops, tg)?,
        Fnstcw => emit(K::Fnstcw, ops, tg)?,
        Fnstsw => ops.push(IrOp::X87 {
            kind: K::Fnstsw,
            addr: Val::Imm(0),
            sti: 0,
        }),
        Fprem => emit(K::Fprem, ops, tg)?,
        // Transcendentals (task-206): f64-precision, ST(0)/ST(1)-implicit (no operand).
        Fsin => emit(K::Fsin, ops, tg)?,
        Fcos => emit(K::Fcos, ops, tg)?,
        Fptan => emit(K::Fptan, ops, tg)?,
        Fpatan => emit(K::Fpatan, ops, tg)?,
        F2xm1 => emit(K::F2xm1, ops, tg)?,
        Fyl2x => emit(K::Fyl2x, ops, tg)?,
        Fyl2xp1 => emit(K::Fyl2xp1, ops, tg)?,
        Fsincos => emit(K::Fsincos, ops, tg)?,
        _ => return Err(unsupported_insn(insn)),
    }
    Ok(())
}
