use super::*;

/// Two-operand ALU lift. Handles the register/immediate/memory destination and the
/// read-modify-write case: for a memory destination the effective address is
/// computed ONCE (§7.1) and reused for Load and Store, with the Store emitted
/// before nothing else commits (atomicity, §16 pitfall #0 — flag recompute on
/// retry is idempotent from the same inputs).
pub(crate) fn lift_binop(
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

        // `lock`-prefixed ALU RMW → one atomic op + a separate flag recompute
        // (§8.2.3, §11). The flag ALU runs on the atomically-read `old`, so locked
        // ops flag exactly like their plain forms.
        if write_back && insn.has_lock_prefix() {
            if let Some(rop) = rmw_of_binop(op) {
                let b = lower_read(insn, 1, ops, tg)?;
                let old = tg.fresh();
                ops.push(IrOp::AtomicRmw {
                    old,
                    addr,
                    src: b,
                    size,
                    op: rop,
                });
                let res = tg.fresh();
                ops.push(mk_binop(op, res, Val::Temp(old), b, size, flags));
                return Ok(());
            }
        }

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

/// `inc`/`dec`: `op0 ± 1`, preserving CF (`ALL_BUT_CF`). RMW-safe via lift_binop's
/// memory path (the immediate 1 is the second source).
/// Shared skeleton for a single-`r/m`-operand op (`inc`/`dec`/`neg`/`not`, task-172):
/// the three destination paths — `lock` → atomic RMW (+ a flag-recompute on the
/// atomically-read `old` when the op sets flags), plain memory → load/compute/store,
/// register → read/compute/write. The op-specific bits are the atomic `(rmw_op,
/// rmw_src)`, whether it recomputes flags, and `emit`, which pushes the non-atomic
/// compute `res = f(a)` (also reused for the atomic flag-recompute with `a = old`).
pub(crate) fn lift_unary_op0(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    rmw_op: RmwOp,
    rmw_src: Val,
    recompute_flags: bool,
    mut emit: impl FnMut(&mut Vec<IrOp>, crate::ir::Temp, Val, u8),
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        if insn.has_lock_prefix() {
            let old = tg.fresh();
            ops.push(IrOp::AtomicRmw {
                old,
                addr,
                src: rmw_src,
                size,
                op: rmw_op,
            });
            if recompute_flags {
                // Recompute flags on the atomically-read `old`; the result is discarded.
                let res = tg.fresh();
                emit(ops, res, Val::Temp(old), size);
            }
            return Ok(());
        }
        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        let res = tg.fresh();
        emit(ops, res, a, size);
        ops.push(IrOp::Store {
            addr,
            src: Val::Temp(res),
            size,
            order: MemOrder::None,
        });
        return Ok(());
    }
    let a = lower_read(insn, 0, ops, tg)?;
    let res = tg.fresh();
    emit(ops, res, a, size);
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(res));
    Ok(())
}

/// `inc`/`dec`: `op0 ± 1`, flags set but CF preserved (§8.2.3).
pub(crate) fn lift_incdec(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: BinOp,
) -> Result<(), LiftError> {
    let rmw = if matches!(op, BinOp::Add) {
        RmwOp::Add
    } else {
        RmwOp::Sub
    };
    lift_unary_op0(
        insn,
        ops,
        tg,
        rmw,
        Val::Imm(1),
        true,
        |ops, res, a, size| {
            ops.push(mk_binop(
                op,
                res,
                a,
                Val::Imm(1),
                size,
                FlagMask::ALL_BUT_CF,
            ));
        },
    )
}

/// `shld`/`shrd`: double-precision shift. op0 (r/m) is the destination + first
/// source, op1 (r) supplies the fill bits, op2 (imm8 or CL) is the count.
pub(crate) fn lift_double_shift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    left: bool,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let b = lower_read(insn, 1, ops, tg)?;
    let count = lower_read(insn, 2, ops, tg)?;
    let res = tg.fresh();
    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        ops.push(IrOp::DoubleShift {
            dst: res,
            a,
            b,
            count,
            size,
            left,
            set_flags: FlagMask::SHIFT,
        });
        ops.push(IrOp::Store {
            addr,
            src: Val::Temp(res),
            size,
            order: MemOrder::None,
        });
    } else {
        let a = lower_read(insn, 0, ops, tg)?;
        ops.push(IrOp::DoubleShift {
            dst: res,
            a,
            b,
            count,
            size,
            left,
            set_flags: FlagMask::SHIFT,
        });
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(res));
    }
    Ok(())
}

/// `neg`: `0 - op0`. Flags exactly as `sub` from zero (CF set iff operand ≠ 0).
pub(crate) fn lift_neg(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    // `0 - op0`: reverse-subtract atomic RMW; flags exactly as `sub` from zero.
    lift_unary_op0(
        insn,
        ops,
        tg,
        RmwOp::Rsub,
        Val::Imm(0),
        true,
        |ops, res, a, size| {
            ops.push(IrOp::Sub {
                dst: res,
                a: Val::Imm(0),
                b: a,
                size,
                set_flags: FlagMask::ALL,
            });
        },
    )
}

/// `not`: bitwise complement, NO flags. Lowered as `xor op0, -1` with an empty
/// flag mask (the result is masked to the operand size by the interpreter).
pub(crate) fn lift_not(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    // Bitwise complement = `op0 ^ -1`, a native atomic XOR under `lock`, no flags.
    lift_unary_op0(
        insn,
        ops,
        tg,
        RmwOp::Xor,
        Val::Imm(u64::MAX),
        false,
        |ops, res, a, size| {
            ops.push(IrOp::Xor {
                dst: res,
                a,
                b: Val::Imm(u64::MAX),
                size,
                set_flags: FlagMask::NONE,
            });
        },
    )
}

/// One-operand `mul`/`imul`: `RDX:RAX = RAX * op0`. 8-bit form writes AH (not
/// expressible), so it's rejected; 16/32/64-bit split into RAX (low) and RDX (high).
pub(crate) fn lift_widening_mul(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    signed: bool,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if size == 1 {
        // 8-bit one-operand form (`mul`/`imul r/m8`, F6 /4,/5): AX = AL * src8, the
        // 16-bit product landing in AH:AL — not the RDX:RAX split of the wider forms.
        // Only AX is written; RAX[63:16] is untouched. CF/OF flag a non-zero high byte
        // (Mul with size 1 sets that exactly). task-189.
        let a = read_reg(Reg::Rax, ops, tg);
        let b = lower_read(insn, 0, ops, tg)?;
        let lo = tg.fresh();
        let hi = tg.fresh();
        ops.push(IrOp::Mul {
            lo,
            hi,
            a,
            b,
            size: 1,
            signed,
            set_flags: FlagMask::CF_OF,
        });
        // AX = (hi << 8) | lo, written as a 16-bit reg so RAX[63:16] is preserved.
        let hi_sh = alu_none(ops, tg, |dst| IrOp::Shl {
            dst,
            a: Val::Temp(hi),
            b: Val::Imm(8),
            size: 2,
            set_flags: FlagMask::NONE,
        });
        let ax = alu_none(ops, tg, |dst| IrOp::Or {
            dst,
            a: Val::Temp(lo),
            b: hi_sh,
            size: 2,
            set_flags: FlagMask::NONE,
        });
        ops.push(IrOp::WriteReg {
            reg: Reg::Rax,
            src: ax,
            size: 2,
        });
        return Ok(());
    }
    let a = read_reg(Reg::Rax, ops, tg);
    let b = lower_read(insn, 0, ops, tg)?;
    let lo = tg.fresh();
    let hi = tg.fresh();
    ops.push(IrOp::Mul {
        lo,
        hi,
        a,
        b,
        size,
        signed,
        set_flags: FlagMask::CF_OF,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rax,
        src: Val::Temp(lo),
        size,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rdx,
        src: Val::Temp(hi),
        size,
    });
    Ok(())
}

/// `imul`: one-operand (`RDX:RAX`), two-operand (`dst *= src`), or three-operand
/// (`dst = src * imm`). The 2/3-operand forms keep only the low half in `dst`;
/// CF/OF still flag overflow of the full signed product.
pub(crate) fn lift_imul(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
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
            ops.push(IrOp::Mul {
                lo,
                hi,
                a,
                b,
                size,
                signed: true,
                set_flags: FlagMask::CF_OF,
            });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(lo));
            Ok(())
        }
        _ => Err(unsupported_insn(insn)),
    }
}

/// `div`/`idiv`: `RDX:RAX / op0` → RAX quotient, RDX remainder. May raise `#DE`
/// (zero divisor / overflow) — the `Div` op traps before the register writes, so a
/// retry sees clean state (§16).
pub(crate) fn lift_div(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    signed: bool,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if size == 1 {
        // 8-bit one-operand form (`div`/`idiv r/m8`, F6 /6,/7): dividend is the 16-bit
        // AX (not the RDX:RAX split of the wider forms) — quotient → AL, remainder → AH.
        // The `Div` op with size 1 reads its `hi:lo` as AH:AL and packs both results the
        // same way; #DE traps before any write. task-248.
        let rax = read_reg(Reg::Rax, ops, tg);
        // The `Div` op reads `hi`/`lo` as AH/AL and masks each to the low byte itself, so
        // `lo` is the raw RAX and `hi` only needs AH shifted into the low byte.
        let hi = alu_none(ops, tg, |dst| IrOp::Shr {
            dst,
            a: rax,
            b: Val::Imm(8),
            size: 8,
            set_flags: FlagMask::NONE,
        });
        let lo = rax;
        let divisor = lower_read(insn, 0, ops, tg)?;
        let quot = tg.fresh();
        let rem = tg.fresh();
        ops.push(IrOp::Div {
            quot,
            rem,
            hi,
            lo,
            divisor,
            size: 1,
            signed,
        });
        // quotient → AL (8-bit WriteReg leaves RAX[63:8] untouched), remainder → AH.
        ops.push(IrOp::WriteReg {
            reg: Reg::Rax,
            src: Val::Temp(quot),
            size: 1,
        });
        emit_write(
            ops,
            tg,
            WriteTarget::HighByte { parent: Reg::Rax },
            Val::Temp(rem),
        );
        return Ok(());
    }
    let hi = read_reg(Reg::Rdx, ops, tg);
    let lo = read_reg(Reg::Rax, ops, tg);
    let divisor = lower_read(insn, 0, ops, tg)?;
    let quot = tg.fresh();
    let rem = tg.fresh();
    ops.push(IrOp::Div {
        quot,
        rem,
        hi,
        lo,
        divisor,
        size,
        signed,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rax,
        src: Val::Temp(quot),
        size,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rdx,
        src: Val::Temp(rem),
        size,
    });
    Ok(())
}

/// `xadd dst, src`: `tmp = dst + src; dst = tmp; src = old_dst`, flags as `add`.
/// A memory destination is atomic (typically `lock`-prefixed, §8.2.3); the source
/// register receives the prior memory value.
pub(crate) fn lift_xadd(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let src = lower_read(insn, 1, ops, tg)?; // source register value

    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let old = tg.fresh();
        ops.push(IrOp::AtomicRmw {
            old,
            addr,
            src,
            size,
            op: RmwOp::Add,
        });
        // flags = add(old, src)
        let res = tg.fresh();
        ops.push(mk_binop(
            BinOp::Add,
            res,
            Val::Temp(old),
            src,
            size,
            FlagMask::ALL,
        ));
        // source register <- old memory value
        let dst1 = lower_write_target(insn, 1, ops, tg)?;
        emit_write(ops, tg, dst1, Val::Temp(old));
        return Ok(());
    }

    // Register destination (non-atomic): dst = dst + src; src = old dst.
    let dst_val = lower_read(insn, 0, ops, tg)?;
    let res = tg.fresh();
    ops.push(mk_binop(BinOp::Add, res, dst_val, src, size, FlagMask::ALL));
    let dst1 = lower_write_target(insn, 1, ops, tg)?;
    emit_write(ops, tg, dst1, dst_val);
    let dst0 = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst0, Val::Temp(res));
    Ok(())
}

/// `cmpxchg dst, src`: compare the accumulator (AL/AX/EAX/RAX) with `dst`; if
/// equal, `dst = src` and ZF=1, else the accumulator takes `dst` and ZF=0. Flags
/// are those of `cmp acc, dst`. A memory destination is atomic (`lock cmpxchg`,
/// §8.2.3) via a single CAS. The register-destination form is deferred (rare, and
/// not a synchronization primitive).
pub(crate) fn lift_cmpxchg(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    if insn.op_kind(0) != OpKind::Memory {
        return Err(unsupported_insn(insn));
    }
    let size = operand_size(insn, 0);
    let addr = effective_address(insn, ops, tg)?;
    let src = lower_read(insn, 1, ops, tg)?;
    // Accumulator, masked to the operand width, is the expected value.
    let acc = read_reg(Reg::Rax, ops, tg);
    let exp = tg.fresh();
    ops.push(IrOp::And {
        dst: exp,
        a: acc,
        b: Val::Imm(size_mask(size)),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    let old = tg.fresh();
    ops.push(IrOp::AtomicCas {
        old,
        addr,
        expected: Val::Temp(exp),
        src,
        size,
    });
    // Flags = cmp(acc, old).
    let res = tg.fresh();
    ops.push(IrOp::Sub {
        dst: res,
        a: Val::Temp(exp),
        b: Val::Temp(old),
        size,
        set_flags: FlagMask::ALL,
    });
    // Accumulator <- old (a no-op on success, the memory value on failure).
    ops.push(IrOp::WriteReg {
        reg: Reg::Rax,
        src: Val::Temp(old),
        size,
    });
    Ok(())
}

/// `bsf`/`bsr`: bit-scan the source into the destination register, setting ZF.
pub(crate) fn lift_bitscan(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: crate::ir::BitScanOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let src = lower_read(insn, 1, ops, tg)?;
    let old = lower_read(insn, 0, ops, tg)?; // preserved when src == 0 (bsf/bsr)
    let t = tg.fresh();
    ops.push(IrOp::BitScan {
        dst: t,
        src,
        old,
        size,
        op,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `bt`/`bts`/`btr`/`btc`: CF ← the addressed bit; the set/reset/complement forms
/// also write the modified operand back.
///
/// The bit index is masked modulo the operand width — *except* a **register** index
/// against a **memory** operand, which x86 treats as a signed bit-string offset:
/// the addressed byte is `base + (index >> 3)` (arithmetic shift, so a negative
/// index reaches below the base) and the bit within it is `index & 7`. An immediate
/// index is always masked to the operand width (Intel SDM), so its memory form keeps
/// the plain operand-width load/store.
pub(crate) fn lift_bt(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: BtOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let bit = lower_read(insn, 1, ops, tg)?;

    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;

        // Register bit index → bit-string addressing at byte granularity; immediate
        // index → masked to the operand width, a plain operand-width access.
        let (ea, esize) = if insn.op_kind(1) == OpKind::Register {
            let idx_size = operand_size(insn, 1);
            // Sign-extend the index to 64 bits, then arithmetic-shift right by 3 to
            // get the (possibly negative) byte displacement from the base address.
            let byte_off = {
                let sext = tg.fresh();
                ops.push(IrOp::Sext {
                    dst: sext,
                    a: bit,
                    from: idx_size,
                });
                let off = tg.fresh();
                ops.push(IrOp::Sar {
                    dst: off,
                    a: Val::Temp(sext),
                    b: Val::Imm(3),
                    size: 8,
                    set_flags: FlagMask::NONE,
                });
                Val::Temp(off)
            };
            // size:1 → the bit index is masked to `& 7` (the bit within the byte).
            (add_addr(addr, byte_off, ops, tg), 1u8)
        } else {
            (addr, size)
        };
        emit_mem_bt(ops, tg, ea, esize, bit, op, insn.has_lock_prefix());
        return Ok(());
    }

    let a = lower_read(insn, 0, ops, tg)?;
    let result = tg.fresh();
    ops.push(IrOp::Bt {
        result,
        a,
        bit,
        size,
        op,
    });
    if !matches!(op, BtOp::Test) {
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(result));
    }
    Ok(())
}

/// Emit the memory-side of `bt`/`bts`/`btr`/`btc` at address `ea`, width `esize`,
/// bit index `bit`, setting CF ← the addressed bit. A **`lock`-prefixed** set/reset/
/// complement compiles to a real atomic RMW (a concurrent `lock bts` on a shared
/// bitmap must not tear the read-modify-write); everything else keeps the plain
/// load-modify-store. `bt` (test) never writes, so its single load is already atomic.
pub(crate) fn emit_mem_bt(
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    ea: Val,
    esize: u8,
    bit: Val,
    op: BtOp,
    locked: bool,
) {
    if locked && !matches!(op, BtOp::Test) {
        // mask = 1 << (bit & (esize*8 - 1)) — the single bit within the accessed unit.
        let width_bits = esize as u64 * 8;
        let shift = {
            let t = tg.fresh();
            ops.push(IrOp::And {
                dst: t,
                a: bit,
                b: Val::Imm(width_bits - 1),
                size: esize,
                set_flags: FlagMask::NONE,
            });
            Val::Temp(t)
        };
        let mask = {
            let t = tg.fresh();
            ops.push(IrOp::Shl {
                dst: t,
                a: Val::Imm(1),
                b: shift,
                size: esize,
                set_flags: FlagMask::NONE,
            });
            Val::Temp(t)
        };
        // set → OR mask; complement → XOR mask; reset → AND ~mask (mask XOR all-ones).
        let (rmw_op, src) = match op {
            BtOp::Set => (RmwOp::Or, mask),
            BtOp::Complement => (RmwOp::Xor, mask),
            BtOp::Reset => {
                let inv = tg.fresh();
                ops.push(IrOp::Xor {
                    dst: inv,
                    a: mask,
                    b: Val::Imm(size_mask(esize)),
                    size: esize,
                    set_flags: FlagMask::NONE,
                });
                (RmwOp::And, Val::Temp(inv))
            }
            BtOp::Test => unreachable!(),
        };
        let old = tg.fresh();
        ops.push(IrOp::AtomicRmw {
            old,
            addr: ea,
            src,
            size: esize,
            op: rmw_op,
        });
        // CF ← the pre-modification bit. `Bt` with `Test` sets CF from `old` and writes
        // nothing; `bit` is masked to the width internally, matching the RMW's `shift`.
        let cf = tg.fresh();
        ops.push(IrOp::Bt {
            result: cf,
            a: Val::Temp(old),
            bit,
            size: esize,
            op: BtOp::Test,
        });
        return;
    }

    let a = {
        let t = tg.fresh();
        ops.push(IrOp::Load {
            dst: t,
            addr: ea,
            size: esize,
        });
        Val::Temp(t)
    };
    let result = tg.fresh();
    ops.push(IrOp::Bt {
        result,
        a,
        bit,
        size: esize,
        op,
    });
    if !matches!(op, BtOp::Test) {
        ops.push(IrOp::Store {
            addr: ea,
            src: Val::Temp(result),
            size: esize,
            order: MemOrder::None,
        });
    }
}

/// `bswap`: reverse the byte order of a 32/64-bit register. No flags.
pub(crate) fn lift_bswap(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let a = lower_read(insn, 0, ops, tg)?;
    let t = tg.fresh();
    ops.push(IrOp::Bswap { dst: t, a, size });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// BMI1/BMI2 single-dst bit op (task-168.5.3): `dst(op0) = op(op1, op2)`. The unary
/// bls* forms have only two operands, so `b` defaults to 0. Reuses `IrOp::Bmi` +
/// `BmiOp` — one seam for the whole family.
pub(crate) fn lift_bmi(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: crate::ir::BmiOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let a = lower_read(insn, 1, ops, tg)?;
    let b = if insn.op_count() >= 3 {
        lower_read(insn, 2, ops, tg)?
    } else {
        Val::Imm(0)
    };
    let t = tg.fresh();
    ops.push(IrOp::Bmi {
        dst: t,
        a,
        b,
        size,
        op,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `mulx dst_hi, dst_lo, src` (BMI2, task-168.5.3): `(hi:lo) = RDX * src`, unsigned,
/// NO flags. Reuses `IrOp::Mul` (which already yields `lo`/`hi` temps). Writing `lo`
/// before `hi` gives the correct `hi` when the two destinations are the same register.
pub(crate) fn lift_mulx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let rdx = read_reg(Reg::Rdx, ops, tg); // implicit multiplier
    let src = lower_read(insn, 2, ops, tg)?;
    let lo = tg.fresh();
    let hi = tg.fresh();
    ops.push(IrOp::Mul {
        lo,
        hi,
        a: rdx,
        b: src,
        size,
        signed: false,
        set_flags: FlagMask::NONE,
    });
    let dlo = lower_write_target(insn, 1, ops, tg)?;
    emit_write(ops, tg, dlo, Val::Temp(lo));
    let dhi = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dhi, Val::Temp(hi));
    Ok(())
}

/// BMI2 flagless shift/rotate (`shlx`/`shrx`/`sarx`/`rorx`, task-168.5.3): a 3-operand
/// `dst = src <op> count` that sets NO flags — just the existing Shl/Shr/Sar/Ror IR op
/// with `FlagMask::NONE`. `mk` builds the specific op.
pub(crate) fn lift_bmi_shift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mk: impl Fn(crate::ir::Temp, Val, Val, u8, FlagMask) -> IrOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let a = lower_read(insn, 1, ops, tg)?;
    let b = lower_read(insn, 2, ops, tg)?; // count (reg) or imm8 (rorx)
    let t = tg.fresh();
    ops.push(mk(t, a, b, size, FlagMask::NONE));
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `movbe`: move with byte swap between a register and memory (task-176). Reuses the
/// existing `Bswap` IR op — no new op — around a `Load`/`Store`. `movbe r, m` loads,
/// swaps, writes the register; `movbe m, r` swaps the register, stores. No flags.
pub(crate) fn lift_movbe(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let addr = effective_address(insn, ops, tg)?;
    if insn.op_kind(1) == OpKind::Memory {
        // movbe reg, [mem]
        let loaded = tg.fresh();
        ops.push(IrOp::Load {
            dst: loaded,
            addr,
            size,
        });
        let swapped = tg.fresh();
        ops.push(IrOp::Bswap {
            dst: swapped,
            a: Val::Temp(loaded),
            size,
        });
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(swapped));
    } else {
        // movbe [mem], reg
        let a = lower_read(insn, 1, ops, tg)?;
        let swapped = tg.fresh();
        ops.push(IrOp::Bswap {
            dst: swapped,
            a,
            size,
        });
        ops.push(IrOp::Store {
            addr,
            src: Val::Temp(swapped),
            size,
            order: MemOrder::None,
        });
    }
    Ok(())
}

/// `xchg dst, src`: swap the two operands. A memory operand makes the swap atomic
/// — on x86 `xchg` with memory is *implicitly* locked (§8.2.3), so it lowers to an
/// atomic exchange (register operand gets the prior memory value). The reg↔reg
/// form is a plain swap. No flags either way.
pub(crate) fn lift_xchg(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    // A memory operand (either position) makes this an atomic exchange; its
    // register partner receives the prior memory value.
    let reg_idx = if insn.op_kind(0) == OpKind::Memory {
        Some(1u32)
    } else if insn.op_kind(1) == OpKind::Memory {
        Some(0u32)
    } else {
        None
    };
    if let Some(reg_idx) = reg_idx {
        let size = operand_size(insn, reg_idx);
        let addr = effective_address(insn, ops, tg)?;
        let reg_val = lower_read(insn, reg_idx, ops, tg)?;
        let old = tg.fresh();
        ops.push(IrOp::AtomicRmw {
            old,
            addr,
            src: reg_val,
            size,
            op: RmwOp::Xchg,
        });
        let dst = lower_write_target(insn, reg_idx, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(old));
        return Ok(());
    }

    let a_val = lower_read(insn, 0, ops, tg)?;
    let b_val = lower_read(insn, 1, ops, tg)?;
    let dst0 = lower_write_target(insn, 0, ops, tg)?;
    let dst1 = lower_write_target(insn, 1, ops, tg)?;
    emit_write(ops, tg, dst0, b_val);
    emit_write(ops, tg, dst1, a_val);
    Ok(())
}

/// `movzx`: zero-extend the source (mask to its width), write with the dst width.
pub(crate) fn lift_movzx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
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
pub(crate) fn lift_movsx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let src_size = operand_size(insn, 1);
    let v = lower_read(insn, 1, ops, tg)?;
    let s = tg.fresh();
    ops.push(IrOp::Sext {
        dst: s,
        a: v,
        from: src_size,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(s));
    Ok(())
}

/// `cdqe`: sign-extend EAX into RAX.
/// `cbw`/`cwde`/`cdqe`: sign-extend the accumulator in place from `from` to `to`
/// bytes (AL→AX, AX→EAX, EAX→RAX). Writing `to=4` zeroes RAX's upper 32 (x86);
/// `to=2` merges into RAX, preserving bits above 16.
pub(crate) fn lift_cbw_family(
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: u8,
    to: u8,
) -> Result<(), LiftError> {
    let rax = read_reg(Reg::Rax, ops, tg);
    let s = tg.fresh();
    ops.push(IrOp::Sext {
        dst: s,
        a: rax,
        from,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rax,
        src: Val::Temp(s),
        size: to,
    });
    Ok(())
}

/// `cqo`: RDX = sign of RAX (arithmetic shift by 63 → all-zero or all-one).
/// `cwd`/`cdq`/`cqo`: fill (D/E/R)DX with the sign of the same-width accumulator
/// (arithmetic shift by width-1). The DX write uses the operand width, so `cdq`
/// zero-extends the upper 32 bits of RDX and `cwd` preserves the upper 48.
pub(crate) fn lift_sign_into_dx(
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    size: u8,
) -> Result<(), LiftError> {
    let rax = read_reg(Reg::Rax, ops, tg);
    let s = tg.fresh();
    ops.push(IrOp::Sar {
        dst: s,
        a: rax,
        b: Val::Imm(size as u64 * 8 - 1),
        size,
        set_flags: FlagMask::NONE,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rdx,
        src: Val::Temp(s),
        size,
    });
    Ok(())
}

/// `setcc r/m8`: materialize the condition as 0/1 into an 8-bit destination.
pub(crate) fn lift_setcc(
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
pub(crate) fn lift_cmovcc(
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
    ops.push(IrOp::Xor {
        dst: diff,
        a: dst_val,
        b: src,
        size: 8,
        set_flags: FlagMask::NONE,
    });
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
