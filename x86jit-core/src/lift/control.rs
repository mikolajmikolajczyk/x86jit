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
    // Real16: SP wraps mod 2^16 (same reason).
    emit_sp_wrap(ops, new_rsp, mode);
    // Real16 (§17.6): the stack lives at SS; the physical store address is
    // `ss_base + (SP & 0xFFFF)`. The wrapped SP is the offset; add the SS base.
    let store_addr = stack_addr(Val::Temp(new_rsp), ops, tg, mode);
    ops.push(IrOp::Store {
        addr: store_addr,
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
///
/// Fault ordering (§16): `pop` is restartable — if the destination write faults, RSP
/// must be left un-advanced (fault-before-commit, matching hardware). So the RSP
/// write-back is ordered relative to the destination write by destination kind:
///
/// * **Register dst** — commit the RSP increment *first*, write the destination
///   register *last*. A register write can't fault, so ordering only matters for
///   `pop rsp`: writing the destination last lets the popped value override the RSP
///   increment.
/// * **Memory dst** — emit the (possibly-faulting) `Store` *first*, RSP write-back
///   *last*. On a store fault RSP is never committed, so a restarted `pop [mem]`
///   re-reads the same stack slot with the original RSP.
pub(crate) fn lift_pop(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mode: CpuMode,
) -> Result<(), LiftError> {
    let size = push_pop_size(insn, mode);
    let rsp = read_reg(Reg::Rsp, ops, tg);
    // Real16 (§17.6): pop reads from `ss_base + (SP & 0xFFFF)`. SP is already a valid
    // 16-bit value at block entry (seeded so, and every SP update re-wraps), so it is
    // used directly as the offset here.
    let load_addr = stack_addr(rsp, ops, tg, mode);
    let val = tg.fresh();
    ops.push(IrOp::Load {
        dst: val,
        addr: load_addr,
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
    // Real16: the new SP wraps mod 2^16 before it is written back (the 2-byte SP write
    // below preserves the upper GPR bits, so the low 16 bits must already be wrapped).
    // Compat32 relies on its 4-byte SP write to zero-extend instead — its IR is
    // unchanged.
    if mode.wraps_16() {
        emit_sp_wrap(ops, new_rsp, mode);
    }
    // A 4-byte RSP write in Compat32 zero-extends → ESP wraps mod 2^32.
    let rsp_writeback = IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: sp_write_size(mode),
    };
    let dst = lower_write_target(insn, 0, ops, tg)?;
    match dst {
        WriteTarget::Mem { .. } => {
            // Store first (can fault), RSP write-back last → restartable on a store
            // fault. The effective address was lowered above from the *pre-pop* RSP; a
            // `pop [rsp+disp]` naming RSP as its own base/index is defined by the SDM
            // against post-increment RSP, but compilers never emit it — assert loudly
            // rather than silently address off the wrong RSP.
            debug_assert!(
                insn.memory_base() != Register::RSP && insn.memory_index() != Register::RSP,
                "pop [mem] with RSP-relative destination address is unsupported",
            );
            emit_write(ops, tg, dst, Val::Temp(val));
            ops.push(rsp_writeback);
        }
        _ => {
            // Register dst (incl. `pop rsp`): commit RSP first, destination last so the
            // popped value overrides the RSP increment.
            ops.push(rsp_writeback);
            emit_write(ops, tg, dst, Val::Temp(val));
        }
    }
    Ok(())
}

/// Near `call` in real mode (§17.6): push the 16-bit return IP onto SS:SP (with SP
/// pre-decremented by 2 and 16-bit-wrapped), then jump to the near target. Only the
/// near forms (`call rel16`, `call r/m16`) are in scope; a far `call` (segment:offset)
/// is a later sub-seam, so reject it loudly rather than mis-execute.
pub(crate) fn lift_call_real16(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    if is_far_flow(insn) {
        return Err(unsupported_insn(insn));
    }
    // Target first (an indirect `call [mem]` reads SS/DS-relative memory off the
    // *pre-push* SP, matching hardware).
    let target = branch_target(insn, ops, tg, CpuMode::Real16)?;
    let return_ip = mask_pc(insn.next_ip(), CpuMode::Real16);

    // SP -= 2, wrapped mod 2^16.
    let sp = read_reg(Reg::Rsp, ops, tg);
    let new_sp = tg.fresh();
    ops.push(IrOp::Sub {
        dst: new_sp,
        a: sp,
        b: Val::Imm(2),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    emit_sp_wrap(ops, new_sp, CpuMode::Real16);
    // Store the return IP at SS:(new SP), then commit SP, then jump.
    let store_addr = stack_addr(Val::Temp(new_sp), ops, tg, CpuMode::Real16);
    ops.push(IrOp::Store {
        addr: store_addr,
        src: Val::Imm(return_ip),
        size: 2,
        order: MemOrder::None,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_sp),
        size: 2,
    });
    ops.push(IrOp::Jump { target });
    Ok(())
}

/// Near `ret` in real mode (§17.6): pop the 16-bit return IP from SS:SP, advance SP by
/// `2 + imm16` (with 16-bit wrap), then jump to it. `ret imm16` adds the caller-cleanup
/// immediate. A far `ret` (`retf`) is out of scope.
pub(crate) fn lift_ret_real16(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    if is_far_flow(insn) {
        return Err(unsupported_insn(insn));
    }
    let pop_extra = if insn.op_count() > 0 {
        insn.immediate16()
    } else {
        0
    };
    let sp = read_reg(Reg::Rsp, ops, tg);
    let load_addr = stack_addr(sp, ops, tg, CpuMode::Real16);
    let ip = tg.fresh();
    ops.push(IrOp::Load {
        dst: ip,
        addr: load_addr,
        size: 2,
    });
    // SP += 2 + imm16, wrapped mod 2^16.
    let new_sp = tg.fresh();
    ops.push(IrOp::Add {
        dst: new_sp,
        a: sp,
        b: Val::Imm(2 + pop_extra as u64),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    emit_sp_wrap(ops, new_sp, CpuMode::Real16);
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_sp),
        size: 2,
    });
    ops.push(IrOp::Jump {
        target: Val::Temp(ip),
    });
    Ok(())
}

/// `true` for a far control transfer (segment:offset) — far `jmp`/`call`/`ret`. These
/// reload CS and are **deferred** from sub-seam (b): the CS-write + `FetchAddr` machinery
/// that `INT`/`IRET` use could carry them, but the far forms fan out (direct `ptr16:16`
/// vs indirect `[mem]`, a 4-byte far-call frame, `retf imm16`) enough that they are left
/// to a later sub-seam to keep this one focused on interrupt delivery (§17.6). They stay
/// `UnknownInstruction`. A far direct `call`/`jmp` carries a `FarBranch16/32` operand;
/// `retf` has its own opcodes.
fn is_far_flow(insn: &Instruction) -> bool {
    matches!(insn.op_kind(0), OpKind::FarBranch16 | OpKind::FarBranch32)
        || matches!(
            insn.code(),
            Code::Retfw | Code::Retfw_imm16 | Code::Retfd | Code::Retfd_imm16
        )
}

/// A string op with its repeat prefix. movs/stos/lods take `rep`; scas/cmps take
/// `repe`/`repne` (both share the F3/F2 prefix bytes with the instruction kind).
pub(crate) fn lift_string(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
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

    // Address size (§17.5): iced encodes it in the implicit RSI/RDI register width —
    // RSI/RDI (8) = 64-bit, ESI/EDI (4) = 32-bit (a `67h` override in long mode or the
    // default in Compat32), SI/DI (2) = 16-bit. The `string_run` loop masks the
    // pointer arithmetic and RCX to this width.
    let idx_reg = match op {
        StrOp::Stos | StrOp::Scas => insn.memory_base(), // ES:[RDI]-only
        _ => insn.memory_base(),                         // movs/lods/cmps: DS:[RSI]
    };
    let addr_bits: u8 = match idx_reg.size() {
        2 => 16,
        4 => 32,
        _ => 64,
    };

    // Segment override on the DS-relative *source* pointer (RSI). Only movs/lods/cmps
    // read from DS:[RSI]; a `fs`/`gs` prefix there redirects the read. ES:[RDI]
    // (stos/scas dest, cmps second operand) is never overridable → base 0. FS/GS base
    // comes from the guest segment-base registers, exactly like `with_segment`.
    let reads_ds_source = matches!(op, StrOp::Movs | StrOp::Lods | StrOp::Cmps);
    let seg_base = if reads_ds_source {
        match insn.segment_prefix() {
            Register::FS => read_reg(Reg::FsBase, ops, tg),
            Register::GS => read_reg(Reg::GsBase, ops, tg),
            _ => Val::Imm(0),
        }
    } else {
        Val::Imm(0)
    };

    ops.push(IrOp::RepString {
        op,
        elem,
        rep,
        addr_bits,
        seg_base,
    });
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
        // `fnstsw`/`fstsw`: `ax` form writes the status word to AX; the memory form
        // (`fnstsw m16`) stores it to [mem]. Distinct kinds so the exec knows whether
        // to touch AX or the effective address.
        Fnstsw => {
            if mem {
                emit(K::FnstswMem, ops, tg)?;
            } else {
                ops.push(IrOp::X87 {
                    kind: K::Fnstsw,
                    addr: Val::Imm(0),
                    sti: 0,
                });
            }
        }
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
