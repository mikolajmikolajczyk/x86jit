use super::*;

/// VEX.128 move: as [`lift_vmov`], but a register destination also clears bits
/// 255:128 of the YMM (task-168.2). A store (mem dest) writes no register.
pub(crate) fn lift_vmov_vex(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    size: u8,
) -> Result<(), LiftError> {
    lift_vmov(insn, ops, tg, size)?;
    if let Some(d) = reg_xmm(insn, 0) {
        ops.push(IrOp::VZeroUpper { reg: d });
    }
    Ok(())
}

/// `vpbroadcast{b,w,d,q}` (task-168.3): replicate the low `elem`-byte element of the
/// XMM (or memory) source across the XMM/YMM destination.
pub(crate) fn lift_broadcast(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    // Destination width: ZMM → 512, YMM → 256, XMM → 128 (EVEX can widen, task-168.5).
    let (dst, width) = if let Some(z) = reg_zmm(insn, 0) {
        (z, 64u16)
    } else if let Some(y) = reg_ymm(insn, 0) {
        (y, 32)
    } else if let Some(x) = reg_xmm(insn, 0) {
        (x, 16)
    } else {
        return Err(unsupported_insn(insn));
    };
    // EVEX `vpbroadcast{d,q}` from a GPR source (covers 128/256/512, unmasked).
    if insn.op_kind(1) == OpKind::Register && !insn.op_register(1).is_xmm() {
        if evex_is_masked(insn) {
            return Err(unsupported_insn(insn));
        }
        let src = lower_read(insn, 1, ops, tg)?;
        ops.push(IrOp::VBroadcastGpr {
            dst,
            src,
            elem,
            width,
        });
        return Ok(());
    }
    // EVEX-512 broadcast from a memory element: load the `elem`-byte scalar and replicate
    // across all 512 bits via the width-generic `VBroadcastGpr` (task-195). glibc's
    // AVX-512 routines broadcast a constant word/dword from `.rodata` (`vpbroadcastw zmm,
    // [rip+k]`). An xmm-source 512-bit broadcast still defers.
    if width == 64 && !evex_is_masked(insn) && insn.op_kind(1) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let t = tg.fresh();
        ops.push(IrOp::Load {
            dst: t,
            addr,
            size: elem,
        });
        ops.push(IrOp::VBroadcastGpr {
            dst,
            src: Val::Temp(t),
            elem,
            width,
        });
        return Ok(());
    }
    // EVEX-512 broadcast from an XMM element (task-215): extract the low `elem` bytes of
    // the xmm source into a temp GPR (VToGpr keeps the low qword; broadcast_elem re-masks
    // to `elem`), then replicate across 512 bits. openssl's rsaz/SHA emits `vpbroadcastq
    // zmm, xmm`. Masked forms still defer.
    if width == 64 && !evex_is_masked(insn) {
        if let Some(src) = reg_xmm(insn, 1) {
            let t = tg.fresh();
            ops.push(IrOp::VToGpr {
                dst: t,
                src,
                size: 8,
            });
            ops.push(IrOp::VBroadcastGpr {
                dst,
                src: Val::Temp(t),
                elem,
                width,
            });
            return Ok(());
        }
    }
    // XMM/memory source: the existing 128/256 path. EVEX-512 and masked forms defer.
    if width == 64 || evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let w256 = width == 32;
    match reg_xmm(insn, 1) {
        Some(src) => ops.push(IrOp::VBroadcast {
            dst,
            src,
            elem,
            w256,
        }),
        None if insn.op_kind(1) == OpKind::Memory => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VBroadcastM {
                dst,
                addr,
                elem,
                w256,
            });
        }
        None => return Err(unsupported_insn(insn)),
    }
    Ok(())
}

/// EVEX lane broadcast `vbroadcast{i,f}{32x2,32x4,32x8,64x2,64x4,128}` (task-214):
/// replicate a `chunk`-byte block (8/16/32) across the dest, masked at `elem` granularity.
/// Register or memory chunk source.
pub(crate) fn lift_broadcast_lane(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    chunk: u8,
    elem: u8,
) -> Result<(), LiftError> {
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let writemask = evex_writemask(insn);
    let zeroing = insn.zeroing_masking();
    if insn.op_kind(1) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VBroadcastLaneM {
            dst,
            addr,
            chunk,
            elem,
            dst_width,
            writemask,
            zeroing,
        });
    } else {
        let src = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VBroadcastLane {
            dst,
            src,
            chunk,
            elem,
            dst_width,
            writemask,
            zeroing,
        });
    }
    Ok(())
}

/// VEX packed shift-by-immediate (task-168.3), 3-operand `dst = a << imm` etc.,
/// dispatching on width. VEX.128 clears the dest's upper 128 bits.
pub(crate) fn lift_vpacked_shift_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
    right: bool,
    arith: bool,
) -> Result<(), LiftError> {
    // Scalar register count `vp{sll,srl,sra}{w,d,q} v,v,xmm` (task-215): the low 64 bits of
    // an xmm shift every lane uniformly. Memory-source count deferred.
    if !is_immediate(insn.op_kind(2)) {
        let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
        let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        let count = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VShiftReg {
            dst,
            a,
            count,
            elem: lane,
            right,
            arith,
            k: evex_writemask(insn).unwrap_or(0),
            zeroing: insn.zeroing_masking(),
            bytes,
        });
        // VShiftReg preserves bits 255:128 (the SSE form must); the VEX/EVEX 128-bit form
        // clears them (task-237). 256-bit results legitimately fill 255:128 (their >256
        // zeroing is handled by the width-aware write), so only the 128-bit form gets it.
        if bytes == 16 {
            ops.push(IrOp::VZeroUpper { reg: dst });
        }
        return Ok(());
    }
    let imm = insn.immediate(2) as u8;
    // EVEX-512 or masked/zeroing forms (task-215) route through the width- and
    // mask-agnostic VMaskedShift; the VEX 128/256 paths keep their existing ops.
    let writemask = evex_writemask(insn);
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    // Memory source (`vpsrlq zmm,[mem],imm`, task-215): load the operand into `dst`, then
    // shift `dst` in place. Only the unmasked form — a masked merge needs the old `dst`
    // preserved, which loading over it would clobber, so masked+memory stays deferred.
    let mem_src = insn.op_kind(1) == OpKind::Memory;
    if mem_src {
        if writemask.is_some() || insn.zeroing_masking() {
            return Err(unsupported_insn(insn));
        }
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VLoadWide { dst, addr, bytes });
    }
    if bytes >= 64 || writemask.is_some() || insn.zeroing_masking() {
        let a = if mem_src {
            dst
        } else {
            vec_operand(insn, 1)
                .ok_or_else(|| unsupported_insn(insn))?
                .0
        };
        ops.push(IrOp::VMaskedShift {
            dst,
            a,
            imm,
            elem: lane,
            right,
            arith,
            k: writemask.unwrap_or(0),
            zeroing: insn.zeroing_masking(),
            bytes,
        });
        return Ok(());
    }
    let d = reg_vec(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = if mem_src {
        d
    } else {
        reg_vec(insn, 1).ok_or_else(|| unsupported_insn(insn))?
    };
    if reg_ymm(insn, 0).is_some() {
        ops.push(IrOp::VPackedShift256 {
            dst: d,
            a,
            imm,
            lane,
            right,
            arith,
        });
    } else {
        ops.push(IrOp::VPackedShift {
            dst: d,
            a,
            imm,
            lane,
            right,
            arith,
        });
        ops.push(IrOp::VZeroUpper { reg: d });
    }
    Ok(())
}

/// AVX2/AVX-512 per-element variable shift `vp{sll,srl,sra}v{w,d,q}` (task-215): shift each
/// `elem`-byte lane of src1 by the count in the matching lane of src2 (register; memory-source
/// count deferred). Any width (128/256/512) + optional EVEX write-masking via `VShiftVar`.
pub(crate) fn lift_vshift_var(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    elem: u8,
    right: bool,
    arith: bool,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let count = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?; // mem count deferred
    ops.push(IrOp::VShiftVar {
        dst,
        a,
        count,
        elem,
        right,
        arith,
        k: evex_writemask(insn).unwrap_or(0),
        zeroing: insn.zeroing_masking(),
        bytes,
    });
    Ok(())
}

/// GFNI wide/masked path (task-215): `vgf2p8{mulb,affineqb,affineinvqb}` on a YMM/ZMM
/// destination or with an EVEX write-mask, routed to the width- and mask-agnostic `VGf2p8`.
/// A memory src2 (openssl's rip-relative constant matrix) routes to `VGf2p8M`, which reads
/// the matrix from memory in the shared helper — so the `dst == src1` aliasing case works
/// without a scratch register. Shares the GF(2⁸) math with the VEX.128 path via `GfniOp`.
fn lift_vgfni_wide(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: GfniOp,
    dst: u8,
    bytes: u16,
) -> Result<(), LiftError> {
    let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let imm = if op == GfniOp::Mulb {
        0
    } else {
        insn.immediate(3) as u8
    };
    let k = evex_writemask(insn).unwrap_or(0);
    let zeroing = insn.zeroing_masking();
    let mode = op as u8;
    if insn.op_kind(2) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VGf2p8M {
            dst,
            a,
            addr,
            imm,
            mode,
            k,
            zeroing,
            bytes,
        });
        return Ok(());
    }
    let b = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VGf2p8 {
        dst,
        a,
        b,
        imm,
        mode,
        k,
        zeroing,
        bytes,
    });
    Ok(())
}

/// VEX bitwise logic dispatching on width: a YMM destination routes to the 256-bit
/// `VLogic256`/`VLogic256M` (task-168.2), else the VEX.128 path (task-168.1).
pub(crate) fn lift_vlogic_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: VLogicOp,
) -> Result<(), LiftError> {
    let Some(d) = reg_ymm(insn, 0) else {
        return lift_vlogic_vex(insn, ops, tg, op);
    };
    let a = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_ymm,
        2,
        |b| ops.push(IrOp::VLogic256 { dst: d, a, b, op }),
        |addr| ops.push(IrOp::VLogic256M {
            dst: d,
            a,
            addr,
            op
        })
    );
    Ok(())
}

/// VEX packed integer arithmetic dispatching on width: YMM → `VPackedBin256`.
pub(crate) fn lift_vpacked_bin_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
    op: PackedBinOp,
) -> Result<(), LiftError> {
    // EVEX masked packed arith `vp{add,sub,min,max,mull}{k}{z}` (task-168.5.5): compute
    // per-lane then merge/zero-mask under `k`. Register src2 only (masked mem-src
    // deferred); any width (128/256/512). glibc's AVX-512 loops mask tail lanes this way.
    if let Some(k) = evex_writemask(insn) {
        let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
        let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VMaskedPacked {
            dst,
            a,
            b,
            op,
            k,
            elem: lane,
            zeroing: insn.zeroing_masking(),
            bytes,
        });
        return Ok(());
    }
    // EVEX 512-bit: width-generic wide packed arith (register or memory src2, task-195).
    // glibc's memcpy-family uses `vpaddq zmm, zmm, [mem]`.
    if let Some(d) = reg_zmm(insn, 0) {
        if evex_is_masked(insn) {
            return Err(unsupported_insn(insn));
        }
        let a = reg_zmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        vec_src_dispatch!(
            insn,
            ops,
            tg,
            reg_zmm,
            2,
            |b| ops.push(IrOp::VPackedWide {
                dst: d,
                a,
                b,
                lane,
                op,
                bytes: 64
            }),
            |addr| ops.push(IrOp::VPackedWideM {
                dst: d,
                a,
                addr,
                lane,
                op,
                bytes: 64
            })
        );
        return Ok(());
    }
    let Some(d) = reg_ymm(insn, 0) else {
        return lift_vpacked_bin_vex(insn, ops, tg, lane, op);
    };
    let a = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_ymm,
        2,
        |b| ops.push(IrOp::VPackedBin256 {
            dst: d,
            a,
            b,
            lane,
            op
        }),
        |addr| ops.push(IrOp::VPackedBin256M {
            dst: d,
            a,
            addr,
            lane,
            op
        })
    );
    Ok(())
}

/// AVX move (`vmovdqu`/`vmovdqa`/`vmovups`/`vmovaps`) dispatching on width: a YMM
/// operand routes to the 256-bit ops (task-168.2), else the VEX.128 path.
pub(crate) fn lift_vmov_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    // EVEX write-masked move `v{k}{z}, v/[mem]` or `[mem]{k}, v` (task-170.1, 168.5.5):
    // blend under the opmask at `elem` granularity. Reg-reg delegates to `VMaskMov`;
    // a memory operand becomes an element-wise `VMaskLoadMem`/`VMaskStoreMem` (masked-off
    // lanes never touch memory — hardware fault suppression).
    if evex_is_masked(insn) {
        let Some(k) = evex_writemask(insn) else {
            return Err(unsupported_insn(insn));
        };
        match (vec_operand(insn, 0), vec_operand(insn, 1)) {
            // reg, reg → masked register blend.
            (Some((dst, bytes)), Some((src, _))) => {
                ops.push(IrOp::VMaskMov {
                    dst,
                    src,
                    k,
                    elem,
                    zeroing: insn.zeroing_masking(),
                    bytes,
                });
                return Ok(());
            }
            // reg, [mem] → masked load.
            (Some((dst, bytes)), None) if insn.op_kind(1) == OpKind::Memory => {
                let addr = effective_address(insn, ops, tg)?;
                ops.push(IrOp::VMaskLoadMem {
                    dst,
                    addr,
                    k,
                    elem,
                    zeroing: insn.zeroing_masking(),
                    bytes,
                });
                return Ok(());
            }
            // [mem], reg → masked store (no zeroing form).
            (None, Some((src, bytes))) if insn.op_kind(0) == OpKind::Memory => {
                let addr = effective_address(insn, ops, tg)?;
                ops.push(IrOp::VMaskStoreMem {
                    src,
                    addr,
                    k,
                    elem,
                    bytes,
                });
                return Ok(());
            }
            _ => return Err(unsupported_insn(insn)),
        }
    }
    // AVX-512: a ZMM operand routes to the unmasked 512-bit ops (task-168.5).
    let (z0, z1) = (reg_zmm(insn, 0), reg_zmm(insn, 1));
    if z0.is_some() || z1.is_some() {
        let (k0, k1) = (insn.op_kind(0), insn.op_kind(1));
        if let Some(d) = z0 {
            if k1 == OpKind::Memory {
                let addr = effective_address(insn, ops, tg)?;
                ops.push(IrOp::VLoadWide {
                    dst: d,
                    addr,
                    bytes: 64,
                });
                return Ok(());
            }
            if let Some(s) = z1 {
                ops.push(IrOp::VMovWide {
                    dst: d,
                    src: s,
                    bytes: 64,
                });
                return Ok(());
            }
        }
        if let Some(s) = z1 {
            if k0 == OpKind::Memory {
                let addr = effective_address(insn, ops, tg)?;
                ops.push(IrOp::VStoreWide {
                    addr,
                    src: s,
                    bytes: 64,
                });
                return Ok(());
            }
        }
        return Err(unsupported_insn(insn));
    }
    let (y0, y1) = (reg_ymm(insn, 0), reg_ymm(insn, 1));
    if y0.is_none() && y1.is_none() {
        return lift_vmov_vex(insn, ops, tg, 16);
    }
    let (k0, k1) = (insn.op_kind(0), insn.op_kind(1));
    if let Some(d) = y0 {
        if k1 == OpKind::Memory {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VLoadWide {
                dst: d,
                addr,
                bytes: 32,
            });
            return Ok(());
        }
        if let Some(s) = y1 {
            ops.push(IrOp::VMovWide {
                dst: d,
                src: s,
                bytes: 32,
            });
            return Ok(());
        }
    }
    if let Some(s) = y1 {
        if k0 == OpKind::Memory {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VStoreWide {
                addr,
                src: s,
                bytes: 32,
            });
            return Ok(());
        }
    }
    Err(unsupported_insn(insn))
}

/// SSE move (movdqa/movdqu/movaps/movups = 16, movq = 8, movd = 4) between
/// xmm/gpr/memory. `movq`/`movd` reg forms move the low `size` bytes and zero the
/// upper part of the destination xmm.
pub(crate) fn lift_vmov(
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
                ops.push(IrOp::VToGpr {
                    dst: t,
                    src: s,
                    size,
                });
                ops.push(IrOp::VFromGpr {
                    dst: d,
                    src: Val::Temp(t),
                    size,
                });
            }
            return Ok(());
        }
        if k1 == OpKind::Register {
            let g = lower_read(insn, 1, ops, tg)?;
            ops.push(IrOp::VFromGpr {
                dst: d,
                src: g,
                size,
            });
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
            ops.push(IrOp::VToGpr {
                dst: t,
                src: s,
                size,
            });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(t));
            return Ok(());
        }
    }
    Err(unsupported_insn(insn))
}

/// SSE bitwise logic (pxor/pand/por/pandn + *ps aliases). Register source only
/// for now (memory source deferred). `dst = op(dst, src)`.
pub(crate) fn lift_vlogic(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: VLogicOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VLogic {
            dst: d,
            a: d,
            b,
            op
        }),
        |addr| ops.push(IrOp::VLogicM { dst: d, addr, op })
    );
    Ok(())
}

/// Packed integer arithmetic `dst = op(dst, src)` (register source only for now).
pub(crate) fn lift_vpacked_bin(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
    op: PackedBinOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VPackedBin {
            dst: d,
            a: d,
            b,
            lane,
            op
        }),
        |addr| ops.push(IrOp::VPackedBinM {
            dst: d,
            addr,
            lane,
            op
        })
    );
    Ok(())
}

/// VEX.128 3-operand bitwise logic (task-168.1): `dst(op0) = op1 OP op2`, reusing
/// the u128 `VLogic` IR (already `dst,a,b`). A YMM operand → `reg_xmm` is `None` →
/// unsupported (deferred to AVX-256, task-168.2). `op2` may be memory.
pub(crate) fn lift_vlogic_vex(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: VLogicOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |b| ops.push(IrOp::VLogic { dst: d, a, b, op }),
        |addr| {
            // `VLogicM` is `dst = op(dst, mem)`; move `a` into `dst` first.
            if d != a {
                ops.push(IrOp::VMov { dst: d, src: a });
            }
            ops.push(IrOp::VLogicM { dst: d, addr, op });
        }
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128 (task-168.2)
    Ok(())
}

/// VEX.128 3-operand packed integer arithmetic (task-168.1): `dst = op1 OP op2` per
/// `lane` bytes, reusing `VPackedBin`. YMM → unsupported (task-168.2).
pub(crate) fn lift_vpacked_bin_vex(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
    op: PackedBinOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |b| ops.push(IrOp::VPackedBin {
            dst: d,
            a,
            b,
            lane,
            op
        }),
        |addr| {
            if d != a {
                ops.push(IrOp::VMov { dst: d, src: a });
            }
            ops.push(IrOp::VPackedBinM {
                dst: d,
                addr,
                lane,
                op,
            });
        }
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128 (task-168.2)
    Ok(())
}

/// EVEX 128-bit unmasked packed integer op (task-168.5 grind). Reuses the VEX.128
/// path — `VZeroUpper` now clears bits 511:128, which is exactly the EVEX.128
/// zero-upper semantics. The 256/512 EVEX widths and masked forms are deferred.
pub(crate) fn lift_evex_packed_bin_128(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
    op: PackedBinOp,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) || reg_ymm(insn, 0).is_some() || reg_zmm(insn, 0).is_some() {
        return Err(unsupported_insn(insn));
    }
    lift_vpacked_bin_vex(insn, ops, tg, lane, op)
}

/// EVEX lane extract `vextracti{32x4,64x2,32x8,64x4}` (task-195): extract `extract_lanes`
/// 128-bit lanes from `op1` (ZMM/YMM) at the imm8-selected position into `op0` (XMM/YMM
/// register; memory dst deferred). Masking deferred.
pub(crate) fn lift_vextract_wide(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    extract_lanes: u8,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (src, src_bytes) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let slots = (src_bytes as u8 / 16) / extract_lanes; // number of extract positions
    let idx = (insn.immediate(2) as u8) & (slots - 1);
    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VExtractLaneWideM {
            src,
            addr,
            idx,
            num_lanes: extract_lanes,
        });
        return Ok(());
    }
    let dst = vec_operand_reg(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VExtractLaneWide {
        dst,
        src,
        idx,
        num_lanes: extract_lanes,
    });
    Ok(())
}

/// EVEX lane insert `vinserti{32x4,64x2,64x4}` (task-168.5.6): insert `insert_lanes`
/// 128-bit lanes from `op2` (register; memory deferred) into `op1` at the imm8-selected
/// position, writing `op0`. Masking deferred.
pub(crate) fn lift_vinsert_wide(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    insert_lanes: u8,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (src, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let (ins, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let slots = (bytes as u8 / 16) / insert_lanes; // number of insert positions
    let idx = (insn.immediate(3) as u8) & (slots - 1);
    ops.push(IrOp::VInsertLaneWide {
        dst,
        src,
        ins,
        idx,
        num_lanes: insert_lanes,
        bytes,
    });
    Ok(())
}

/// EVEX `valign{d,q}` (task-168.5.6): shift the `src1:src2` concatenation by an imm8
/// element count. Register src2 only (memory deferred); masking deferred.
pub(crate) fn lift_valign(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    elem: u8,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let shift = insn.immediate(3) as u8;
    ops.push(IrOp::VAlign {
        dst,
        a,
        b,
        shift,
        elem,
        bytes,
    });
    Ok(())
}

/// SSE4.1 variable blend `blendvps`/`blendvpd`/`pblendvb` (task-168.5.4). The blend mask
/// is the implicit XMM0; `dst = op0`, blend source `op1` (register or memory).
pub(crate) fn lift_blendv(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VPBlendV { dst, src, lane }),
        |addr| ops.push(IrOp::VPBlendVM { dst, addr, lane })
    );
    Ok(())
}

/// AVX `vblendv{ps,pd}` / `vpblendvb` (task-215): the VEX 4-operand variable blend —
/// dst, src1, src2, and the blend-control mask are all explicit registers. 128-bit register
/// form (memory src2 deferred).
pub(crate) fn lift_vblendv(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    lane: u8,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?; // mem src2 deferred
    let mask = reg_xmm(insn, 3).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPBlendVX {
        dst,
        a,
        b,
        mask,
        lane,
    });
    Ok(())
}

/// SSE4.1 `round{ps,pd,ss,sd}` (task-168.5.4): round `op1` (register or memory) into
/// `op0` per the imm8 rounding mode.
pub(crate) fn lift_round(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
    scalar: bool,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let mode = insn.immediate(2) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VPRound {
            dst,
            a: dst,
            src,
            prec,
            mode,
            scalar
        }),
        |addr| ops.push(IrOp::VPRoundM {
            dst,
            addr,
            prec,
            mode,
            scalar
        })
    );
    Ok(())
}

/// VEX.128 packed `vround{ps,pd}` (task-242): `dst = round(op1)` per the imm8 mode over
/// every lane, plus VEX's upper-zeroing. Since every lane is overwritten, the merge base
/// is irrelevant — pass `dst` as `a`. A YMM operand → `reg_xmm` is `None` → unsupported.
pub(crate) fn lift_vround(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let mode = insn.immediate(2) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VPRound {
            dst,
            a: dst,
            src,
            prec,
            mode,
            scalar: false
        }),
        |addr| ops.push(IrOp::VPRoundM {
            dst,
            addr,
            prec,
            mode,
            scalar: false
        })
    );
    ops.push(IrOp::VZeroUpper { reg: dst }); // VEX.128 clears bits 255:128
    Ok(())
}

/// VEX.128 scalar `vround{ss,sd}` (task-242): 3-operand round — round op2's low element
/// under the imm8 rounding-control bits, take the bits above the element from op1, and
/// clear bits 255:128. Same shape as `vrndscale{ss,sd}` with M=0.
pub(crate) fn lift_vround_scalar(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let mode = insn.immediate(3) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        // Register op2: `VPRound` reads `a` (merge base = op1) and `src` before writing
        // dst, so a src aliasing dst is safe — no pre-copy of op1 into dst (task-203).
        |src| ops.push(IrOp::VPRound {
            dst,
            a,
            src,
            prec,
            mode,
            scalar: true
        }),
        // Memory op2: `VPRoundM` merges into `dst` in place, so op1 must be in dst first.
        // Memory can't alias a register, so this copy is safe.
        |addr| {
            if dst != a {
                ops.push(IrOp::VMov { dst, src: a });
            }
            ops.push(IrOp::VPRoundM {
                dst,
                addr,
                prec,
                mode,
                scalar: true,
            });
        }
    );
    ops.push(IrOp::VZeroUpper { reg: dst }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `pcmpistri`/`pcmpestri` (+ VEX) → ECX index + flags (task-168.5.4). Source 2 is a
/// register or, for the memory form (task-195), `[addr]` loaded as a 128-bit value.
pub(crate) fn lift_pcmpstr_idx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    explicit: bool,
) -> Result<(), LiftError> {
    let a = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    if let Some(b) = reg_xmm(insn, 1) {
        ops.push(IrOp::VPcmpStr {
            a,
            b,
            imm,
            explicit,
        });
    } else {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VPcmpStrM {
            a,
            addr,
            imm,
            explicit,
        });
    }
    Ok(())
}

/// `pcmpistrm`/`pcmpestrm` (+ VEX) → XMM0 mask + flags (task-195). Same operand shape as
/// [`lift_pcmpstr_idx`] (op0, op1/[mem], imm8); the result is a mask in XMM0 instead of an
/// index in ECX. Source 2 is a register or, for the memory form, `[addr]` loaded as 128 bits.
pub(crate) fn lift_pcmpstr_mask(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    explicit: bool,
) -> Result<(), LiftError> {
    let a = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    if let Some(b) = reg_xmm(insn, 1) {
        ops.push(IrOp::VPcmpStrMask {
            a,
            b,
            imm,
            explicit,
        });
    } else {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VPcmpStrMaskM {
            a,
            addr,
            imm,
            explicit,
        });
    }
    Ok(())
}

/// SSE4.1 `insertps xmm, xmm/m32, imm8` (task-195): insert one dword into a dst lane and
/// optionally zero lanes. `dst` is also source 1. Register source is another xmm; the memory
/// form is a 32-bit load. Inlined in codegen (lane moves + zeroing).
pub(crate) fn lift_insertps(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VInsertPs { dst, src, imm }),
        |addr| ops.push(IrOp::VInsertPsM { dst, addr, imm })
    );
    Ok(())
}

/// SSE4.1 `dpps xmm, xmm/m128, imm8` (task-195): single-precision dot product. `dst` is also
/// source 1. Register or m128 source 2. Horizontal FP sum → shared helper (jit == interp).
pub(crate) fn lift_dpps(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VDpps { dst, b, imm }),
        |addr| ops.push(IrOp::VDppsM { dst, addr, imm })
    );
    Ok(())
}

/// EVEX scalar `vrndscale{ss,sd}` (task-195). For scale factor M=0 (imm8[7:4]==0) the
/// operation is a 3-operand `round{ss,sd}`: round op2's low element under the imm8[3:0]
/// rounding-control bits, take bits above the element from op1, and clear bits 255:128.
/// Scaled (M≠0) and write-masked forms are deferred.
pub(crate) fn lift_vrndscale(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let imm = insn.immediate(3) as u8;
    if imm >> 4 != 0 {
        return Err(unsupported_insn(insn)); // non-zero scale factor deferred
    }
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    // imm8[3:0] is the same rounding-control encoding `round{ss,sd}` uses.
    let mode = imm & 0x0f;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        // Register op2: `VPRound` reads `a` (merge base = op1) and `src` before writing
        // dst, so a src aliasing dst is safe — no pre-copy of op1 into dst (task-203).
        |src| ops.push(IrOp::VPRound {
            dst,
            a,
            src,
            prec,
            mode,
            scalar: true
        }),
        // Memory op2: `VPRoundM` merges into `dst` in place, so op1 must be in dst
        // first. Memory can't alias a register, so this copy is safe.
        |addr| {
            if dst != a {
                ops.push(IrOp::VMov { dst, src: a });
            }
            ops.push(IrOp::VPRoundM {
                dst,
                addr,
                prec,
                mode,
                scalar: true,
            });
        }
    );
    ops.push(IrOp::VZeroUpper { reg: dst }); // EVEX clears bits 255:128
    Ok(())
}

/// SSE4.1 `pmovzx`/`pmovsx` (task-168.5.4): extend `16/to` low `from`-byte elements to
/// `to` bytes each into `dst`. Source is a register (its low bytes) or memory.
pub(crate) fn lift_pmovx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: u8,
    to: u8,
    signed: bool,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VPMovExtend {
            dst,
            src,
            from,
            to,
            signed
        }),
        |addr| ops.push(IrOp::VPMovExtendM {
            dst,
            addr,
            from,
            to,
            signed
        })
    );
    Ok(())
}

/// VEX-128 `vpmov{z,s}x*` (task-195): the SSE zero/sign-extend plus VEX's upper-zeroing.
/// A YMM destination (256-bit extend) → `reg_xmm` is `None` in `lift_pmovx` → unsupported.
pub(crate) fn lift_vpmovx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: u8,
    to: u8,
    signed: bool,
) -> Result<(), LiftError> {
    // Wide (ymm/zmm) dest — EVEX/VEX-256 — or a masked xmm dest: route to the shared
    // widening helper (register src only; memory-src wide forms deferred). glibc's v4
    // routines use `vpmovsxdq zmm, ymm` to widen dword indices to qword.
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    if dst_width > 16 || evex_is_masked(insn) {
        let src = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VPMovExtendWide {
            dst,
            src,
            from,
            to,
            signed,
            dst_width,
            writemask: evex_writemask(insn),
            zeroing: insn.zeroing_masking(),
        });
        return Ok(());
    }
    // 128-bit dest (SSE4.1 VEX-128): inline extend + VEX upper-zeroing.
    lift_pmovx(insn, ops, tg, from, to, signed)?;
    ops.push(IrOp::VZeroUpper { reg: dst }); // VEX.128 clears bits 255:128
    Ok(())
}

/// Packed absolute value `vpabs{b,w,d,q}` (VEX/EVEX, task-195): `dst = |src|` per
/// `elem`-byte lane, any width, masked/zeroing. Register src only (memory-src deferred);
/// `vec_operand` gives the dest width (= VL), above which EVEX zeroes.
pub(crate) fn lift_vpabs(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    elem: u8,
) -> Result<(), LiftError> {
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let src = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPAbs {
        dst,
        src,
        elem,
        dst_width,
        writemask: evex_writemask(insn),
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// Masked EVEX unary lane op `vplzcnt{d,q}` / `vprol{d,q}` / `vpconflict{d,q}` (task-209):
/// `dst = f(src)` per `elem`-byte lane, any width, masked/zeroing. `vprol` carries an
/// `imm8` (operand 2); the others have none. Register src only.
pub(crate) fn lift_vp_unary_lane(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: crate::ir::VpUnaryOp,
    elem: u8,
) -> Result<(), LiftError> {
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let src = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let imm = if op == crate::ir::VpUnaryOp::Rol {
        insn.immediate(2) as u8
    } else {
        0
    };
    ops.push(IrOp::VpUnaryLane {
        dst,
        src,
        op,
        imm,
        elem,
        dst_width,
        writemask: evex_writemask(insn),
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// Masked EVEX blend `vpblendm{d,q}` (task-209): `dst[i] = k[i] ? b[i] : a[i]` per
/// `elem`-byte lane (zeroing → masked-off lanes 0). The opmask is the blend control.
/// Register srcs only.
pub(crate) fn lift_vp_blendm(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    // Memory src2 (openssl EVEX blend): load it into dst, then blend with b = dst. Valid
    // because dst != a there (the aliasing case is deferred).
    let b = match vec_operand_reg(insn, 2) {
        Some(b) => b,
        None if insn.op_kind(2) == OpKind::Memory && dst != a => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VLoadWide {
                dst,
                addr,
                bytes: dst_width,
            });
            dst
        }
        None => return Err(unsupported_insn(insn)),
    };
    ops.push(IrOp::VpBlendm {
        dst,
        a,
        b,
        k: evex_writemask(insn).unwrap_or(0),
        elem,
        dst_width,
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// Masked EVEX 128-bit-lane shuffle `vshuff32x4` / `vshuff64x2` (task-209): imm8 selects
/// whole 128-bit lanes from the two sources. `elem` (4/8) is the masking granularity.
/// Register srcs only.
pub(crate) fn lift_vshuf_lane(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    elem: u8,
) -> Result<(), LiftError> {
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(3) as u8;
    ops.push(IrOp::VShuffLane {
        dst,
        a,
        b,
        imm,
        elem,
        dst_width,
        writemask: evex_writemask(insn),
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// Masked EVEX `vpmultishiftqb` (AVX512-VBMI, task-209): per-qword unaligned byte gather.
/// `ctrl` = src1 (shift indices), `data` = src2. Masked at byte granularity. Register
/// srcs only.
pub(crate) fn lift_vp_multishift(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let ctrl = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let data = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VpMultishift {
        dst,
        ctrl,
        data,
        dst_width,
        writemask: evex_writemask(insn),
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// AVX512-VPOPCNTDQ `vpopcnt{d,q}` (task-195): per-lane population count over 128/256/512
/// bits, register or memory source. Masked forms are deferred.
pub(crate) fn lift_vpopcnt(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        1,
        |a| ops.push(IrOp::VPopcnt {
            dst,
            a,
            lane,
            bytes
        }),
        |addr| ops.push(IrOp::VPopcntM {
            dst,
            addr,
            lane,
            bytes
        })
    );
    Ok(())
}

/// `vpermt2{b,w,d,q}` (task-195): two-table cross-lane permute. iced op order is (dst,
/// idx, tbl); `dst` is also table 0 (its old value). Register src only (memory deferred).
pub(crate) fn lift_vpermt2(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    lift_vperm2(insn, ops, tg, elem, false)
}

/// `vpermi2{b,w,d,q}` (task-195): index-mode two-table permute — the OLD `dst` is the
/// index and `src1`/`src2` are the two tables (t-mode swaps index and table 0).
pub(crate) fn lift_vpermi2(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    lift_vperm2(insn, ops, tg, elem, true)
}

pub(crate) fn lift_vperm2(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
    imode: bool,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let idx = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let writemask = evex_writemask(insn);
    let zeroing = insn.zeroing_masking();
    // Table 1 (op2) is a register or a memory operand.
    if insn.op_kind(2) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VPermT2M {
            dst,
            idx,
            addr,
            elem,
            writemask,
            zeroing,
            bytes,
            imode,
        });
        return Ok(());
    }
    let tbl = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPermT2 {
        dst,
        idx,
        tbl,
        elem,
        writemask,
        zeroing,
        bytes,
        imode,
    });
    Ok(())
}

/// EVEX narrowing move `vpmov{q,d,w}{d,w,b}` (task-195): truncate each `from`-byte
/// source lane to `to` bytes. `src` (op1) carries the vector width; the destination
/// (op0) must be a register — `vec_operand_reg` returns `None` for the memory-dest form,
/// leaving it deferred.
pub(crate) fn lift_vpmov_narrow(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: u8,
    to: u8,
) -> Result<(), LiftError> {
    let (src, src_width) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    // Memory destination (unmasked): truncate + store contiguously. Masked memory-dest
    // (per-lane fault suppression) is deferred.
    if insn.op_kind(0) == OpKind::Memory {
        if evex_is_masked(insn) {
            return Err(unsupported_insn(insn));
        }
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VPmovNarrowMem {
            src,
            addr,
            from,
            to,
            src_width,
        });
        return Ok(());
    }
    let dst = vec_operand_reg(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPmovNarrow {
        dst,
        src,
        from,
        to,
        src_width,
        writemask: evex_writemask(insn),
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// `kunpck{bw,wd,dq}` (task-195): interleave two opmasks into a wider one — `k[dst] =
/// (k[a]_low << half) | k[b]_low`. iced op order is (dst, src1=a, src2=b).
pub(crate) fn lift_kunpck(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    half: u8,
) -> Result<(), LiftError> {
    let dst = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_kmask(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VKUnpack { dst, a, b, half });
    Ok(())
}

/// Opmask bitwise logic `k{or,and,andn,xor,xnor}{b,w,d,q}` (task-195): `k[dst] =
/// op(k[a], k[b])` over the low `width` bits. iced op order is (dst, src1=a, src2=b).
pub(crate) fn lift_kbinop(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: VKLogicOp,
    width: u8,
) -> Result<(), LiftError> {
    let dst = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_kmask(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VKBinOp {
        dst,
        a,
        b,
        op,
        width,
    });
    Ok(())
}

/// Opmask complement `knot{b,w,d,q}` (task-195): `k[dst] = ~k[a]` over `width` bits.
pub(crate) fn lift_knot(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    width: u8,
) -> Result<(), LiftError> {
    let dst = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VKNot { dst, a, width });
    Ok(())
}

/// Opmask shift `kshift{l,r}{b,w,d,q}` (task-195): `k[dst] = k[a] {<<,>>} imm8` within the
/// low `width` bits. iced op order is (dst, src, imm8).
pub(crate) fn lift_kshift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    width: u8,
    left: bool,
) -> Result<(), LiftError> {
    let dst = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let amount = insn.immediate(2) as u8;
    ops.push(IrOp::VKShift {
        dst,
        a,
        amount,
        width,
        left,
    });
    Ok(())
}

/// EVEX bitwise logic `vpxor{d,q}` / `vpand{d,q}` / `vpor{d,q}` / `vpandn{d,q}`
/// (task-168.5.2). Width-generic (128/256/512) via [`IrOp::VLogicWide`]; the `d`/`q`
/// suffix only picks the mask granularity, irrelevant unmasked. Register src2 only;
/// masked forms are deferred (they belong with the masked-EVEX-data-op work, 168.5.5).
pub(crate) fn lift_evex_vlogic(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: VLogicOp,
    elem: u8,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    // A write-mask (k1–k7) selects the masked form; k0/none is plain unmasked logic. The
    // `d`/`q` suffix sets the masking granularity (`elem` = 4 or 8 bytes) (task-168.5.5).
    if let Some(k) = evex_writemask(insn) {
        // Masked memory-source logic is deferred; masked reg-src only.
        let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VMaskedLogic {
            dst,
            a,
            b,
            op,
            k,
            elem,
            zeroing: insn.zeroing_masking(),
            bytes,
        });
        return Ok(());
    }
    // Unmasked: register or memory src2 (task-195).
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |b| ops.push(IrOp::VLogicWide {
            dst,
            a,
            b,
            op,
            bytes
        }),
        |addr| ops.push(IrOp::VLogicWideM {
            dst,
            a,
            addr,
            op,
            bytes
        })
    );
    Ok(())
}

/// EVEX `vpternlog{d,q}` (task-168.5.2): 3-input bitwise logic via an 8-bit truth table.
/// `dst` is both the first source and the destination; `src3` register only (memory
/// deferred); masked forms deferred.
pub(crate) fn lift_vpternlog(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (b, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(3) as u8;
    // src3 is a register or a memory vector (task-195).
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |c| ops.push(IrOp::VPTernlog {
            dst,
            b,
            c,
            imm,
            bytes
        }),
        |addr| ops.push(IrOp::VPTernlogM {
            dst,
            b,
            addr,
            imm,
            bytes
        })
    );
    Ok(())
}

/// `kmov{b,w,d,q}` between opmask, GPR, and memory (task-168.5). `width` in bits.
pub(crate) fn lift_kmov(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    width: u8,
) -> Result<(), LiftError> {
    // Destination is an opmask: from another opmask, or a GPR/memory source.
    if let Some(k) = reg_kmask(insn, 0) {
        if let Some(sk) = reg_kmask(insn, 1) {
            ops.push(IrOp::VKMovKK {
                dst: k,
                src: sk,
                width,
            });
            return Ok(());
        }
        let src = lower_read(insn, 1, ops, tg)?;
        ops.push(IrOp::VKFromGpr { k, src, width });
        return Ok(());
    }
    // Destination is a GPR/memory, source is an opmask.
    if let Some(k) = reg_kmask(insn, 1) {
        let t = tg.fresh();
        ops.push(IrOp::VKToGpr { dst: t, k, width });
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(t));
        return Ok(());
    }
    Err(unsupported_insn(insn))
}

/// `kortest{b,w,d,q}`: OR two opmasks and set ZF/CF (task-168.5).
pub(crate) fn lift_kortest(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    width: u8,
) -> Result<(), LiftError> {
    let a = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VKOrTest { a, b, width });
    Ok(())
}

/// EVEX `vpcmp{,u}{b,w,d,q}` → opmask (task-168.5). `dst = k`, `src1 = op1` (vvvv),
/// `src2 = op2`, predicate = imm8. Register src2 only; memory + write-masked forms
/// deferred.
/// EVEX `vptestm{b,w,d,q}` / `vptestnm{b,w,d,q}` → opmask (task-168.5.4): `k = (a & b)`
/// per-lane test (or its negation for `nm`). Register sources (memory deferred). glibc's
/// AVX-512 `strlen`/`memchr` use `vptestnmb` to locate zero bytes.
pub(crate) fn lift_vptest(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
    neg: bool,
) -> Result<(), LiftError> {
    let k = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, width) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let writemask = evex_writemask(insn);
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |b| ops.push(IrOp::VPTestToMask {
            k,
            a,
            b,
            elem,
            width,
            neg,
            writemask
        }),
        |addr| ops.push(IrOp::VPTestToMaskM {
            k,
            a,
            addr,
            elem,
            width,
            neg,
            writemask
        })
    );
    Ok(())
}

pub(crate) fn lift_vpcmp(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
    signed: bool,
) -> Result<(), LiftError> {
    let k = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, width) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let pred = insn.immediate(3) as u8;
    // EVEX write-mask k1–k7 (k0 = unmasked); vpcmp uses it as a compare predicate.
    let writemask = evex_writemask(insn);
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |b| ops.push(IrOp::VPCmpToMask {
            k,
            a,
            b,
            elem,
            width,
            pred,
            signed,
            writemask
        }),
        |addr| ops.push(IrOp::VPCmpToMaskM {
            k,
            a,
            addr,
            elem,
            width,
            pred,
            signed,
            writemask
        })
    );
    Ok(())
}

/// Dedicated-opcode compares `vpcmpeq{b,w,d}` / `vpcmpgt{b,w,d}` (task-168.5.1). iced
/// shares each mnemonic between the legacy/VEX packed form (xmm/ymm destination, a
/// per-lane all-ones/zero mask *in a vector*) and the EVEX form (opmask `k` destination
/// with a write-mask, one bit per lane). Distinguish by the destination: a `k` register is
/// the EVEX form — route it to the vpcmp→mask machinery ([`IrOp::VPCmpToMask`]) with the
/// opcode's fixed predicate (`EQ` / signed `GT`); anything else is the packed form.
/// glibc's string/memcmp routines are the heaviest user of the EVEX form. Register src2
/// only, matching [`lift_vpcmp`] (a memory source is deferred).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lift_vpcmp_fixed_or_packed(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
    packed: PackedBinOp,
    pred: u8,
    signed: bool,
) -> Result<(), LiftError> {
    let Some(k) = reg_kmask(insn, 0) else {
        return lift_vpacked_bin_avx(insn, ops, tg, elem, packed);
    };
    let (a, width) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let writemask = evex_writemask(insn);
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |b| ops.push(IrOp::VPCmpToMask {
            k,
            a,
            b,
            elem,
            width,
            pred,
            signed,
            writemask
        }),
        |addr| ops.push(IrOp::VPCmpToMaskM {
            k,
            a,
            addr,
            elem,
            width,
            pred,
            signed,
            writemask
        })
    );
    Ok(())
}

/// Packed shift by immediate `dst = dst << imm` / `>> imm` per lane; a right shift
/// is arithmetic when `arith` (psra*). The register-count form (variable shift) is
/// deferred.
pub(crate) fn lift_vpacked_shift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    lane: u8,
    right: bool,
    arith: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    if !is_immediate(insn.op_kind(1)) {
        // Register-count form `psll/psrl/psra {w,d,q} xmm, xmm` (task-237): the low 64 bits
        // of the count xmm shift every lane uniformly (x86 over-shift → 0 / sign fill). The
        // native JIT path lowers this to a vector shift; the interp uses `exec_shift_reg`.
        // Memory-count (`psll* xmm, m128`) is deferred, like the AVX 3-operand path.
        let count = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VShiftReg {
            dst: d,
            a: d,
            count,
            elem: lane,
            right,
            arith,
            k: 0,
            zeroing: false,
            bytes: 16,
        });
        return Ok(());
    }
    let imm = insn.immediate(1) as u8;
    ops.push(IrOp::VPackedShift {
        dst: d,
        a: d,
        imm,
        lane,
        right,
        arith,
    });
    Ok(())
}

/// `psrldq`/`pslldq`: byte-shift the whole 128-bit register by an immediate,
/// right when `right` else left.
pub(crate) fn lift_byteshift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    right: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let bytes = insn.immediate(1) as u8;
    ops.push(IrOp::VByteShift {
        dst: d,
        a: d,
        bytes,
        right,
    });
    Ok(())
}

/// VEX.128 `vpsrldq`/`vpslldq` (task-195): 3-operand `dst = a shifted by imm8 bytes`,
/// then clear bits 255:128. `reg_xmm` on op0/op1 keeps this VEX.128-only.
pub(crate) fn lift_byteshift_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    right: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let bytes = insn.immediate(2) as u8;
    ops.push(IrOp::VByteShift {
        dst: d,
        a,
        bytes,
        right,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `pshufd`: permute the four 32-bit lanes by imm8 (register source only).
pub(crate) fn lift_pshufd(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    // Memory source: load into `dst`, then shuffle it in place.
    let a = match reg_xmm(insn, 1) {
        Some(a) => a,
        None if insn.op_kind(1) == OpKind::Memory => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VLoad {
                dst: d,
                addr,
                size: 16,
            });
            d
        }
        None => return Err(unsupported_insn(insn)),
    };
    ops.push(IrOp::VShuffle32 { dst: d, a, imm });
    Ok(())
}

/// VEX.128 `vpshufd xmm, xmm/m, imm8` (task-168): the SSE dword shuffle plus VEX's
/// upper-zeroing. A YMM/EVEX form → `reg_xmm` is `None` in `lift_pshufd` → unsupported
/// (256-bit defers). glibc/coreutils emit the VEX-128 form freely once AVX is on.
/// Single-source cross-lane permute `vperm{d,q}` (vector-index, task-195): register src
/// only (memory src deferred). `vec_operand` gives the width; masked/zeroing supported.
pub(crate) fn lift_vperm1(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let idx = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let writemask = evex_writemask(insn);
    let zeroing = insn.zeroing_masking();
    if insn.op_kind(2) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VPerm1M {
            dst,
            idx,
            addr,
            elem,
            bytes,
            writemask,
            zeroing,
        });
    } else {
        let src = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VPerm1 {
            dst,
            idx,
            src,
            elem,
            bytes,
            writemask,
            zeroing,
        });
    }
    Ok(())
}

/// VEX/EVEX `vpack{ss,us}{wb,dw}` (task-195): 3-operand saturating pack, register src2.
/// Any width; the helper's `set_vec` zeroes bits above the register (VEX/EVEX semantics).
pub(crate) fn lift_vpack(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from_elem: u8,
    signed: bool,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    // Memory src2 is 128-bit only (`VPackWideM`); the wide (ymm) mem form is deferred.
    if bytes == 16 && insn.op_kind(2) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        if dst != a {
            ops.push(IrOp::VMov { dst, src: a });
        }
        ops.push(IrOp::VPackWideM {
            dst,
            addr,
            from_elem,
            signed,
        });
        // VEX.128: VPackWideM writes only the low 128 bits; clear bits 255:128.
        ops.push(IrOp::VZeroUpper { reg: dst });
        return Ok(());
    }
    let b = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPackWide {
        dst,
        a,
        b,
        from_elem,
        signed,
        bytes,
    });
    Ok(())
}

/// VEX.128 `vpblendw` (task-195): 3-operand per-word imm8 blend + upper-zeroing. Register
/// src2 only (memory src deferred).
pub(crate) fn lift_vpblendw(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(3) as u8;
    ops.push(IrOp::VBlendW { dst, a, b, imm });
    ops.push(IrOp::VZeroUpper { reg: dst }); // VEX.128 clears bits 255:128
    Ok(())
}

pub(crate) fn lift_vpblendd(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(3) as u8;
    ops.push(IrOp::VBlendD {
        dst,
        a,
        b,
        imm,
        bytes,
    });
    Ok(())
}

pub(crate) fn lift_vpshufd(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    // Wide (ymm/zmm) or masked → shared per-lane helper (register src only; memory src
    // for the wide form is deferred). python3 hits `vpshufd ymm`.
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    if bytes > 16 || evex_is_masked(insn) {
        let a = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        let imm = insn.immediate(2) as u8;
        ops.push(IrOp::VShuffle32Wide {
            dst,
            a,
            imm,
            bytes,
            writemask: evex_writemask(insn),
            zeroing: insn.zeroing_masking(),
        });
        return Ok(());
    }
    lift_pshufd(insn, ops, tg)?;
    ops.push(IrOp::VZeroUpper { reg: dst }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `shufps`/`shufpd`: interleave two 32-bit (resp. 64-bit) lanes from `dst` with
/// two from `src`. `shufpd`'s 2-bit imm is expanded to the `shufps` selector so
/// one IR op (`VShufps`) covers both.
pub(crate) fn lift_shufps(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    let imm32 = if insn.mnemonic() == Mnemonic::Shufpd {
        let lo = (imm & 1) * 2; // 64-bit lane -> its two 32-bit lanes
        let hi = ((imm >> 1) & 1) * 2;
        lo | ((lo + 1) << 2) | (hi << 4) | ((hi + 1) << 6)
    } else {
        imm
    };
    ops.push(IrOp::VShufps {
        dst: d,
        a: d,
        b,
        imm: imm32,
    });
    Ok(())
}

/// `vpermilps`/`vpermilpd` with an imm8 control, VEX.128 form (`vpermil{ps,pd} xmm,
/// xmm/m128, imm8`). Both are single-source in-lane permutes, so they lower to the
/// existing dword shuffle (`VShuffle32`): `vpermilps`'s imm is already a 4×2-bit dword
/// selector; `vpermilpd`'s 2 one-bit selectors (pick double 0 or 1) expand to the same
/// dword form (each double = its two dwords). The 256-bit/EVEX forms (per-lane control)
/// and the variable-control form (`0F38 0C/0D`, control in a vector) are deferred —
/// `reg_xmm`/the operand-kind guard return unsupported so they surface as a clean trap.
/// openssl's rsaz keygen emits the VEX.128 memory-source `vpermilpd` (task-215).
pub(crate) fn lift_vpermil_imm(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    is_pd: bool,
) -> Result<(), LiftError> {
    // imm-control form only (the variable form's op2 is a vector, not an immediate).
    if insn.op_count() != 3 || insn.op_kind(2) != OpKind::Immediate8 {
        return Err(unsupported_insn(insn));
    }
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    // Memory source: load into `dst`, then shuffle it in place (mirrors lift_pshufd).
    let a = match reg_xmm(insn, 1) {
        Some(a) => a,
        None if insn.op_kind(1) == OpKind::Memory => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VLoad {
                dst: d,
                addr,
                size: 16,
            });
            d
        }
        None => return Err(unsupported_insn(insn)),
    };
    let imm32 = if is_pd {
        let s0 = (imm & 1) * 2; // double 0 source -> its low dword
        let s1 = ((imm >> 1) & 1) * 2; // double 1 source -> its low dword
        s0 | ((s0 + 1) << 2) | (s1 << 4) | ((s1 + 1) << 6)
    } else {
        imm // vpermilps: imm is already the dword selector
    };
    ops.push(IrOp::VShuffle32 {
        dst: d,
        a,
        imm: imm32,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `pshuflw` (`high`=false) / `pshufhw` (`high`=true): word permute of one 64-bit
/// half. Register source only.
pub(crate) fn lift_pshufw(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    high: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    ops.push(IrOp::VShuffle16 {
        dst: d,
        a,
        imm,
        high,
    });
    Ok(())
}

/// `punpckl*`: interleave the low halves of dst and src at `lane`-byte elements.
pub(crate) fn lift_vunpack(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
    high: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    // In-place SSE form (`dst == a`); `VUnpackLow{M}` reads `a`=dst before writing dst.
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VUnpackLow {
            dst: d,
            a: d,
            b,
            lane,
            high
        }),
        |addr| ops.push(IrOp::VUnpackLowM {
            dst: d,
            addr,
            lane,
            high
        })
    );
    Ok(())
}

/// VEX.128 `vpunpck{l,h}{bw,wd,dq,qdq}` (task-195, mem src task-243): 3-operand interleave
/// `dst = unpack(a, b)` then clear bits 255:128. `b` may be a register or a 128-bit memory
/// operand (rip-relative loads land here — Mono emits `vpunpckldq [rip+…], xmm, xmm`). A
/// YMM operand → `reg_xmm` returns `None` (per-128-lane semantics) → deferred.
pub(crate) fn lift_vunpack_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
    high: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        // Register src2: `VUnpackLow` reads `a` and `b` before writing dst, so a src
        // aliasing dst is safe — no pre-copy.
        |b| ops.push(IrOp::VUnpackLow {
            dst: d,
            a,
            b,
            lane,
            high
        }),
        // Memory src2: `VUnpackLowM` unpacks `dst` in place, so op1 must be in dst first.
        // Memory can't alias a register, so this copy is safe.
        |addr| {
            if d != a {
                ops.push(IrOp::VMov { dst: d, src: a });
            }
            ops.push(IrOp::VUnpackLowM {
                dst: d,
                addr,
                lane,
                high,
            });
        }
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// SSE AES round `op xmm1, xmm2/m128` (in-place): `xmm1 = f(xmm1, xmm2/m128)`.
/// `VAes`/`VAesM` read `a` (=dst) before writing dst, so the in-place form is safe.
pub(crate) fn lift_aes(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: AesOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VAes {
            dst: d,
            a: d,
            b,
            op
        }),
        |addr| ops.push(IrOp::VAesM {
            dst: d,
            a: d,
            addr,
            op
        })
    );
    Ok(())
}

/// VEX.128 AES round `vop xmm1, xmm2, xmm3/m128`: `dst = f(op1, op2)`, bits 255:128
/// cleared. `VAes`/`VAesM` read `a`=op1 (and the reg/mem key) before writing dst, so a
/// key register that aliases dst is safe — no pre-copy of op1 into dst (task-205).
pub(crate) fn lift_vaes(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: AesOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |b| ops.push(IrOp::VAes { dst: d, a, b, op }),
        |addr| ops.push(IrOp::VAesM {
            dst: d,
            a,
            addr,
            op
        })
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `aesimc` (SSE 2-operand) / `vaesimc` (VEX.128 2-operand): `dst = InvMixColumns(src)`.
/// The VEX form clears bits 255:128.
pub(crate) fn lift_aes_imc(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    vex: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VAesImc { dst: d, src }),
        |addr| ops.push(IrOp::VAesImcM { dst: d, addr })
    );
    if vex {
        ops.push(IrOp::VZeroUpper { reg: d });
    }
    Ok(())
}

/// `aeskeygenassist` (SSE) / `vaeskeygenassist` (VEX.128), 2-operand + imm8.
/// The VEX form clears bits 255:128.
pub(crate) fn lift_aes_keygen(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    vex: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VAesKeygen { dst: d, src, imm }),
        |addr| ops.push(IrOp::VAesKeygenM { dst: d, addr, imm })
    );
    if vex {
        ops.push(IrOp::VZeroUpper { reg: d });
    }
    Ok(())
}

/// `pclmulqdq xmm1, xmm2/m128, imm8` (SSE 2-operand + imm8, in-place: a=dst). `VPclmul`
/// reads `a` (=dst) and the reg/mem op2 before writing dst → in-place is safe (task-211).
pub(crate) fn lift_pclmul(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VPclmul {
            dst: d,
            a: d,
            b,
            imm
        }),
        |addr| ops.push(IrOp::VPclmulM {
            dst: d,
            a: d,
            addr,
            imm
        })
    );
    Ok(())
}

/// VEX.128 `vpclmulqdq xmm1, xmm2, xmm3/m128, imm8`: `dst = clmul(op1, op2, imm)`, bits
/// 255:128 cleared. `VPclmul`/`VPclmulM` read `a`=op1 (and the reg/mem op2) before writing
/// dst, so an op2 register that aliases dst is safe — no pre-copy of op1 (task-211).
pub(crate) fn lift_vpclmul(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(3) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |b| ops.push(IrOp::VPclmul { dst: d, a, b, imm }),
        |addr| ops.push(IrOp::VPclmulM {
            dst: d,
            a,
            addr,
            imm
        })
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// SHA-NI op `sha... xmm1, xmm2/m128[, imm8]` (SSE 2-operand, in-place: a=dst).
/// `sha256rnds2` reads xmm0 implicitly at runtime (the helper loads `cpu.xmm[0]`),
/// so it is not an operand here; `sha1rnds4` carries its `imm8` in `imm`. `VSha`
/// reads `a` (=dst) and the reg/mem source before writing dst → in-place is safe.
pub(crate) fn lift_sha(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: ShaOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    // `sha1rnds4` has an imm8 as its third operand; the others have none.
    let imm = if op == ShaOp::Sha1Rnds4 {
        insn.immediate(2) as u8
    } else {
        0
    };
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VSha {
            dst: d,
            a: d,
            b,
            imm,
            op
        }),
        |addr| ops.push(IrOp::VShaM {
            dst: d,
            a: d,
            addr,
            imm,
            op
        })
    );
    Ok(())
}

/// SSE GFNI `op xmm1, xmm2/m128[, imm8]` (in-place: a=dst). `gf2p8mulb` has no imm8;
/// the affine ops take `imm8` as their third operand. `VGfni`/`VGfniM` read `a` (=dst)
/// and the reg/mem source before writing dst → the in-place form is safe.
pub(crate) fn lift_gfni(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: GfniOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    // The affine ops carry an imm8 as the last (2nd, 0-based) operand; mulb has none.
    let imm = if op == GfniOp::Mulb {
        0
    } else {
        insn.immediate(2) as u8
    };
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VGfni {
            dst: d,
            a: d,
            b,
            imm,
            op
        }),
        |addr| ops.push(IrOp::VGfniM {
            dst: d,
            a: d,
            addr,
            imm,
            op
        })
    );
    Ok(())
}

/// VEX.128 GFNI `vop xmm1, xmm2, xmm3/m128[, imm8]`: `dst = f(op1, op2[, imm8])`, bits
/// 255:128 cleared. The affine ops carry `imm8` as the 4th (index-3) operand. `VGfni`
/// reads `a`=op1 and the reg/mem source before writing dst → a source register aliasing
/// dst is safe (no pre-copy).
pub(crate) fn lift_vgfni(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: GfniOp,
) -> Result<(), LiftError> {
    // YMM/ZMM or masked EVEX forms route through the wide `VGf2p8`; the VEX.128 path below
    // keeps its existing `VGfni`/`VGfniM` ops (task-215).
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    if bytes > 16 || evex_writemask(insn).is_some() || insn.zeroing_masking() {
        return lift_vgfni_wide(insn, ops, tg, op, dst, bytes);
    }
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let imm = if op == GfniOp::Mulb {
        0
    } else {
        insn.immediate(3) as u8
    };
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |b| ops.push(IrOp::VGfni {
            dst: d,
            a,
            b,
            imm,
            op
        }),
        |addr| ops.push(IrOp::VGfniM {
            dst: d,
            a,
            addr,
            imm,
            op
        })
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// SSSE3 `psign{b,w,d} xmm1, xmm2/m128` (in-place): `xmm1[i] = sign(ctrl[i]) applied to
/// xmm1[i]` at `lane`-byte granularity, where ctrl = op2. `a` = src (= dst), `b` = ctrl.
/// `VPsign`/`VPsignM` read both sources before writing dst → the in-place form is safe.
pub(crate) fn lift_psign(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VPsign {
            dst: d,
            a: d,
            b,
            lane
        }),
        |addr| ops.push(IrOp::VPsignM {
            dst: d,
            a: d,
            addr,
            lane
        })
    );
    Ok(())
}

/// VEX.128 `vpsign{b,w,d} xmm1, xmm2, xmm3/m128`: `dst = sign(ctrl) applied to op1`, bits
/// 255:128 cleared. `a` = op1 (src), `b` = op2 (ctrl). Reads both sources before writing
/// dst → a ctrl register aliasing dst is safe (no pre-copy).
pub(crate) fn lift_vpsign(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |b| ops.push(IrOp::VPsign { dst: d, a, b, lane }),
        |addr| ops.push(IrOp::VPsignM {
            dst: d,
            a,
            addr,
            lane
        })
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `packuswb`: pack dst+src 16-bit lanes to unsigned-saturated bytes.
pub(crate) fn lift_packuswb(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPackUsWB { dst: d, a: d, b });
    Ok(())
}

/// Legacy SSE2 signed pack `packsswb`/`packssdw` (task-190): 2-operand (dst == src1),
/// register src2. Reuses the shared `VPackWide` saturating-pack helper (jit == interp).
pub(crate) fn lift_pack_signed(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from_elem: u8,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    // In-place SSE form (`dst == a`); `VPackWide{M}` reads `a`=dst before writing dst.
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VPackWide {
            dst: d,
            a: d,
            b,
            from_elem,
            signed: true,
            bytes: 16,
        }),
        |addr| ops.push(IrOp::VPackWideM {
            dst: d,
            addr,
            from_elem,
            signed: true,
        })
    );
    Ok(())
}

/// Legacy SSE2 `pmaddwd` (task-190): 2-operand (dst == src1), register src2.
/// Cold → shared `VPMAddWd` helper (jit == interp).
pub(crate) fn lift_pmaddwd(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPMAddWd { dst: d, a: d, b });
    Ok(())
}

/// `pinsrw`: insert the low 16 bits of a GPR/memory source into a word lane.
pub(crate) fn lift_pinsrw(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let src = lower_read(insn, 1, ops, tg)?;
    let index = insn.immediate(2) as u8;
    ops.push(IrOp::VInsertW { dst: d, src, index });
    Ok(())
}

/// `pinsrb`/`pinsrd`/`pinsrq` (+ VEX `vpinsr{b,d,q}`): insert the low `size` bytes
/// of a GPR/memory source into `size`-byte lane `index`. Legacy is 2-operand
/// (in-place); the VEX form is 3-operand (`dst = src1 with lane inserted`) and
/// zeroes bits 255:128 (task-168.5 grind).
pub(crate) fn lift_pinsr(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    size: u8,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let vex = insn.op_count() == 4;
    let (base, src_idx, imm_idx) = if vex {
        (
            reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?,
            2,
            3,
        )
    } else {
        (d, 1, 2)
    };
    let src = lower_read(insn, src_idx, ops, tg)?;
    let index = insn.immediate(imm_idx) as u8;
    ops.push(IrOp::VInsertLane {
        dst: d,
        base,
        src,
        index,
        size,
    });
    if vex {
        ops.push(IrOp::VZeroUpper { reg: d });
    }
    Ok(())
}

/// `movlhps`/`movhlps`: copy a 64-bit half between two xmm registers.
pub(crate) fn lift_move_half(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    dst_high: bool,
    src_high: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let s = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VMoveHalf {
        dst: d,
        src: s,
        dst_high,
        src_high,
    });
    Ok(())
}

/// `movhps`/`movlps`: load a 64-bit half from memory into an xmm (`xmm, m64`) or
/// store it (`m64, xmm`). `high` selects the upper vs lower quadword.
pub(crate) fn lift_half_mem(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    high: bool,
) -> Result<(), LiftError> {
    if let Some(d) = reg_xmm(insn, 0) {
        if insn.op_kind(1) == OpKind::Memory {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VLoadHalf { dst: d, addr, high });
            return Ok(());
        }
    }
    if let Some(s) = reg_xmm(insn, 1) {
        if insn.op_kind(0) == OpKind::Memory {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VStoreHalf { addr, src: s, high });
            return Ok(());
        }
    }
    Err(unsupported_insn(insn))
}

/// VEX `vmov{l,h}p{s,d}` (task-195). Two shapes: the store `[mem], xmm` (operand-identical
/// to SSE) and the 3-operand load `xmm, xmm, m64` (bits from the merge source `op1`, the
/// half loaded from `op2`, VEX zeroing bits 255:128).
pub(crate) fn lift_vhalf_mem(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    high: bool,
) -> Result<(), LiftError> {
    // Store form: `[mem], xmm` — reuse the SSE lowering (no upper-zeroing on a store).
    if insn.op_kind(0) == OpKind::Memory {
        return lift_half_mem(insn, ops, tg, high);
    }
    // Load form: `xmm, xmm(merge), m64`.
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    if insn.op_kind(2) != OpKind::Memory {
        return Err(unsupported_insn(insn));
    }
    let addr = effective_address(insn, ops, tg)?;
    if d != a {
        ops.push(IrOp::VMov { dst: d, src: a });
    }
    ops.push(IrOp::VLoadHalf { dst: d, addr, high });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `pextrw dst_gpr, xmm, imm8`: extract a word lane, zero-extended into the gpr.
pub(crate) fn lift_pextrw(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    let src = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let index = insn.immediate(2) as u8;
    let t = tg.fresh();
    ops.push(IrOp::VExtractW { dst: t, src, index });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `pextrb/pextrd/pextrq r/m, xmm, imm8`: extract a `size`-byte lane, zero-extended
/// into a gpr or written to memory.
pub(crate) fn lift_pextr(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    size: u8,
) -> Result<(), LiftError> {
    let src = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let index = insn.immediate(2) as u8;
    let t = tg.fresh();
    ops.push(IrOp::VExtractLane {
        dst: t,
        src,
        index,
        size,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// Read a scalar float operand (xmm low lane or memory) as its raw `prec`-wide
/// bits in a `Val` — used by the compare/convert lifts, which consume only the
/// low lane and want it as an integer value, not a whole xmm.
pub(crate) fn read_scalar_float(
    insn: &Instruction,
    op_idx: u32,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<Val, LiftError> {
    if let Some(x) = reg_xmm(insn, op_idx) {
        let t = tg.fresh();
        ops.push(IrOp::VToGpr {
            dst: t,
            src: x,
            size: prec.bytes(),
        });
        return Ok(Val::Temp(t));
    }
    if insn.op_kind(op_idx) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let t = tg.fresh();
        ops.push(IrOp::Load {
            dst: t,
            addr,
            size: prec.bytes(),
        });
        return Ok(Val::Temp(t));
    }
    Err(unsupported_insn(insn))
}

/// `movss`/`movsd` (xmm forms): reg→reg merges the low lane preserving the upper
/// bytes; the mem forms zero-extend (load) / store the low lane.
pub(crate) fn lift_scalar_fmove(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    let size = prec.bytes();
    if let Some(d) = reg_xmm(insn, 0) {
        if let Some(s) = reg_xmm(insn, 1) {
            ops.push(IrOp::VFloatMov {
                dst: d,
                a: d, // SSE 2-operand: dst supplies the upper bytes
                src: s,
                prec,
            });
            return Ok(());
        }
        if insn.op_kind(1) == OpKind::Memory {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VLoad { dst: d, addr, size });
            return Ok(());
        }
    }
    if let Some(s) = reg_xmm(insn, 1) {
        if insn.op_kind(0) == OpKind::Memory {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VStore { addr, src: s, size });
            return Ok(());
        }
    }
    Err(unsupported_insn(insn))
}

/// VEX `vmovs{s,d}` (task-195). Three shapes: store `[mem], xmm`; 2-operand load `xmm,
/// m` (dst = the loaded scalar, all upper bits zeroed); and 3-operand register merge
/// `xmm, xmm, xmm` (low element from `op2`, bits 127:64 from `op1`, bits 255:128 zeroed).
pub(crate) fn lift_vscalar_fmove(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    let size = prec.bytes();
    // Store form: `[mem], xmm` — plain scalar store, no register write.
    if insn.op_kind(0) == OpKind::Memory {
        let s = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VStore { addr, src: s, size });
        return Ok(());
    }
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    // 2-operand memory load: the scalar goes to the low element, all upper bits zero.
    if insn.op_kind(1) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VLoad { dst: d, addr, size });
        ops.push(IrOp::VZeroUpper { reg: d }); // VEX zeroes bits 255:128 (VLoad zeroes 127:size)
        return Ok(());
    }
    // 3-operand register merge: bits 127:64 from `op1`, low element from `op2`.
    // `VFloatMov` reads `a`=op1 (upper) and `src`=op2 (low) before writing dst, so a
    // src aliasing dst is safe — no pre-copy of op1 into dst (task-203).
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VFloatMov {
        dst: d,
        a,
        src: b,
        prec,
    });
    ops.push(IrOp::VZeroUpper { reg: d });
    Ok(())
}

/// Scalar/packed float arithmetic `dst = op(dst, src)` (register or memory source).
pub(crate) fn lift_float_bin(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: FloatBinOp,
    prec: FPrec,
    scalar: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VFloatBin {
            dst: d,
            a: d,
            b,
            op,
            prec,
            scalar
        }),
        |addr| ops.push(IrOp::VFloatBinM {
            dst: d,
            addr,
            op,
            prec,
            scalar
        })
    );
    Ok(())
}

/// VEX `v{add,sub,mul,div,min,max}{ss,sd,ps,pd}` 128-bit (task-195): 3-operand `dst =
/// op(op1, op2)`. Pre-copy `op1` into `dst` so the SSE `VFloatBin`/`VFloatBinM` lowering
/// (which treats the destination as the first source and, for a scalar op, keeps bits
/// 127:64) sees the right merge base; then VEX zeroes bits 255:128. `op2` may be memory.
/// A YMM operand → `reg_xmm` is `None` → unsupported (256-bit defers).
pub(crate) fn lift_vfloat_bin(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: FloatBinOp,
    prec: FPrec,
    scalar: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        // Register op2: `VFloatBin` is non-destructive — it reads both sources into
        // locals before writing `dst`, and a scalar op keeps bits 127:64 from `a`.
        // So pass op1/op2 straight through, no pre-copy. (The old code did `VMov
        // d←op1` then `VFloatBin { a: d, b: op2 }`, which corrupted the result
        // whenever op2 aliased dst — e.g. CPython's `vaddsd xmm0, xmm1, xmm0` in
        // `_PyLong_Frexp`: the copy clobbered op2 before it was read, yielding
        // `op1+op1` instead of `op1+op2`, so `float(2**30)` came out 0.0. task-202.)
        |b| ops.push(IrOp::VFloatBin {
            dst: d,
            a,
            b,
            op,
            prec,
            scalar
        }),
        // Memory op2: `VFloatBinM` treats `dst` as op1, so op1 must sit in `dst`
        // first. Memory can't alias a vector register, so this copy is safe.
        |addr| {
            if d != a {
                ops.push(IrOp::VMov { dst: d, src: a });
            }
            ops.push(IrOp::VFloatBinM {
                dst: d,
                addr,
                op,
                prec,
                scalar,
            });
        }
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// Legacy SSE3 `h{add,sub}p{s,d}` / `addsubp{s,d}` (task-244): 2-operand `dst =
/// op(dst, src)` over the packed lanes. `src` may be a register or a 128-bit memory
/// operand. `VHFloat`/`VHFloatM` read `a`=dst before writing dst, so in-place is safe.
pub(crate) fn lift_hfloat(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: HFloatOp,
    prec: FPrec,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VHFloat {
            dst: d,
            a: d,
            b,
            op,
            prec
        }),
        |addr| ops.push(IrOp::VHFloatM {
            dst: d,
            addr,
            op,
            prec
        })
    );
    Ok(())
}

/// VEX.128 `vh{add,sub}p{s,d}` / `vaddsubp{s,d}` (task-244): 3-operand `dst = op(op1,
/// op2)` + bits 255:128 cleared. `op2` may be a register or a 128-bit memory operand.
/// `VHFloat` is non-destructive (reads both sources first), so no pre-copy for the reg
/// form. A YMM operand → `reg_xmm` is `None` → unsupported (256-bit defers).
pub(crate) fn lift_vhfloat(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: HFloatOp,
    prec: FPrec,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |b| ops.push(IrOp::VHFloat {
            dst: d,
            a,
            b,
            op,
            prec
        }),
        // Memory op2: `VHFloatM` treats `dst` as op1, so op1 must sit in `dst` first.
        // Memory can't alias a register, so this copy is safe.
        |addr| {
            if d != a {
                ops.push(IrOp::VMov { dst: d, src: a });
            }
            ops.push(IrOp::VHFloatM {
                dst: d,
                addr,
                op,
                prec,
            });
        }
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// Legacy SSSE3 `ph{add,sub}{w,d,sw}` (task-247): 2-operand `dst = op(dst, src)` combining
/// adjacent lane pairs. `src` may be a register or a 128-bit memory operand. `VHInt`/
/// `VHIntM` read `a`=dst before writing dst, so in-place is safe.
pub(crate) fn lift_hint(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: HIntOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |b| ops.push(IrOp::VHInt {
            dst: d,
            a: d,
            b,
            op
        }),
        |addr| ops.push(IrOp::VHIntM { dst: d, addr, op })
    );
    Ok(())
}

/// VEX.128 `vph{add,sub}{w,d,sw}` (task-247): 3-operand `dst = op(op1, op2)` + bits 255:128
/// cleared. `op2` may be a register or a 128-bit memory operand. `VHInt` is non-destructive
/// (reads both sources first), so no pre-copy for the reg form. A YMM operand → `reg_xmm`
/// is `None` → unsupported (256-bit defers).
pub(crate) fn lift_vhint(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: HIntOp,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |b| ops.push(IrOp::VHInt { dst: d, a, b, op }),
        // Memory op2: `VHIntM` treats `dst` as op1, so op1 must sit in `dst` first.
        // Memory can't alias a register, so this copy is safe.
        |addr| {
            if d != a {
                ops.push(IrOp::VMov { dst: d, src: a });
            }
            ops.push(IrOp::VHIntM { dst: d, addr, op });
        }
    );
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `ucomis*`/`comis*`: compare the low lanes and set the arithmetic flags.
pub(crate) fn lift_float_cmp(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    let a = read_scalar_float(insn, 0, ops, tg, prec)?;
    let b = read_scalar_float(insn, 1, ops, tg, prec)?;
    ops.push(IrOp::VFloatCmp { a, b, prec });
    Ok(())
}

/// `cmp{ss,sd,ps,pd}`: per-lane float compare with a predicate imm → mask.
/// Register source only.
pub(crate) fn lift_float_cmp_mask(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    prec: FPrec,
    scalar: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let pred = insn.immediate(2) as u8;
    ops.push(IrOp::VFloatCmpMask {
        dst: d,
        a: d,
        b,
        prec,
        scalar,
        pred,
    });
    Ok(())
}

/// `cvt{,u}si2s*`: integer (gpr/mem) → float in the destination's low lane. `signed`
/// picks the signed `cvtsi2s*` vs the AVX-512 unsigned `cvtusi2s*` form (task-195).
pub(crate) fn lift_cvt_from_int(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let int_size = operand_size(insn, 1);
    let src = lower_read(insn, 1, ops, tg)?;
    ops.push(IrOp::VCvtFromInt {
        dst: d,
        src,
        int_size,
        prec,
        signed: true,
    });
    Ok(())
}

/// VEX/EVEX `vcvt{,u}si2s{s,d} xmm, xmm, r/m` (task-195): 3-operand int→scalar-float. The
/// result's bits 127:64 come from `op1` (the merge source), the low element is the
/// converted integer at `op2`, and the upper bits above 128 are zeroed. Copy `op1` into
/// `dst` first so `VCvtFromInt` (which preserves the upper bits) leaves the right merge.
pub(crate) fn lift_vcvt_from_int(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
    signed: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let int_size = operand_size(insn, 2);
    let src = lower_read(insn, 2, ops, tg)?;
    if d != a {
        ops.push(IrOp::VMov { dst: d, src: a });
    }
    ops.push(IrOp::VCvtFromInt {
        dst: d,
        src,
        int_size,
        prec,
        signed,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX/EVEX clears bits 255:128
    Ok(())
}

/// `cvt(t)s*2si`: float (xmm/mem) → signed integer in the destination GPR.
pub(crate) fn lift_cvt_to_int(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
    trunc: bool,
) -> Result<(), LiftError> {
    lift_cvt_to_int_signed(insn, ops, tg, prec, trunc, true)
}

/// `cvt(t)s*2usi` (AVX-512, task-195): float → **unsigned** integer in a GPR. Same shape
/// as the signed `*2si` form; `signed = false` picks the unsigned saturating cast.
pub(crate) fn lift_cvt_to_int_signed(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
    trunc: bool,
    signed: bool,
) -> Result<(), LiftError> {
    let int_size = operand_size(insn, 0);
    let src = read_scalar_float(insn, 1, ops, tg, prec)?;
    let t = tg.fresh();
    ops.push(IrOp::VCvtToInt {
        dst: t,
        src,
        int_size,
        prec,
        trunc,
        signed,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `sqrts*`/`sqrtp*`: scalar (low lane, upper preserved) or packed square root.
/// Register source (memory source deferred).
pub(crate) fn lift_float_unary(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: FloatUnOp,
    prec: FPrec,
    scalar: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let s = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VFloatUnary {
        dst: d,
        a: d, // SSE in-place: dst is the merge base
        src: s,
        op,
        prec,
        scalar,
    });
    Ok(())
}

/// FMA3 `vf[n]m{add,sub}{132,213,231}{ss,sd,ps,pd}` (task-201): resolve the 132/213/231
/// operand order into `x`/`y`/`z` roles (op0=dst, op1=vvvv, op2=reg/mem), then emit a
/// fused multiply-add. `neg_prod`/`neg_add` pick the sign. Masked EVEX forms are deferred.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lift_fma(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    order: u16,
    prec: FPrec,
    scalar: bool,
    neg_prod: bool,
    neg_add: bool,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let op1 = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let mem = insn.op_kind(2) == OpKind::Memory;
    let op2 = if mem {
        0
    } else {
        vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?
    };
    // EVEX write-masking (task-201 AC#3): `k1`-`k7` mask at element granularity, merge or
    // (with {z}) zero the masked-off lanes. `None` for VEX / EVEX-k0 (unmasked). Packed
    // forms only — masked *scalar* FMA (upper-bits-from-op1 semantics) stays deferred.
    let writemask = evex_writemask(insn);
    if scalar && writemask.is_some() {
        return Err(unsupported_insn(insn));
    }
    let zeroing = insn.zeroing_masking();
    // op0=dst, op1, op2. 132: dst*op2+op1; 213: op1*dst+op2; 231: op1*op2+dst. The memory
    // operand is always op2 → it lands in y (132/231) or z (213).
    let (x, y, z, mem_role) = match order {
        132 => (dst, op2, op1, 1u8),
        213 => (op1, dst, op2, 2u8),
        _ => (op1, op2, dst, 1u8), // 231
    };
    if mem {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VFmaM {
            dst,
            x,
            y,
            z,
            addr,
            mem_role,
            prec,
            scalar,
            neg_prod,
            neg_add,
            bytes,
            writemask,
            zeroing,
        });
    } else {
        ops.push(IrOp::VFma {
            dst,
            x,
            y,
            z,
            prec,
            scalar,
            neg_prod,
            neg_add,
            bytes,
            writemask,
            zeroing,
        });
    }
    Ok(())
}

/// VEX scalar float-unary `vsqrt{ss,sd}` (task-195): 3-operand — the low element is
/// `op(op2)`, bits above it come from op1, and bits 255:128 are cleared. Register src2.
pub(crate) fn lift_vfloat_unary_scalar(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: FloatUnOp,
    prec: FPrec,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let s = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    // `VFloatUnary` reads `a` (merge base = op1) and `src`=op2 before writing dst, so a
    // src aliasing dst is safe — no pre-copy of op1 into dst (task-203).
    ops.push(IrOp::VFloatUnary {
        dst: d,
        a,
        src: s,
        op,
        prec,
        scalar: true,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// `cvtss2sd`/`cvtsd2ss`: convert the low-lane float between precisions.
pub(crate) fn lift_cvt_float(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: FPrec,
    to: FPrec,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let src = read_scalar_float(insn, 1, ops, tg, from)?;
    ops.push(IrOp::VCvtFloat {
        dst: d,
        src,
        from,
        to,
    });
    Ok(())
}

/// VEX scalar `vcvtss2sd`/`vcvtsd2ss` (task-195): 3-operand — bits above the low
/// element come from `op1`, the converted low element from `op2`, bits 255:128 cleared.
/// Register or memory op2; `reg_xmm` on op0/op1 keeps this VEX.128-only.
pub(crate) fn lift_vcvt_scalar(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: FPrec,
    to: FPrec,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let src = read_scalar_float(insn, 2, ops, tg, from)?;
    // Merge op1's upper bits into dst, then overwrite the low element via the convert
    // (VCvtFloat preserves dst[127:size]). Order matters when d == a: the VMov is a no-op.
    if d != a {
        ops.push(IrOp::VMov { dst: d, src: a });
    }
    ops.push(IrOp::VCvtFloat {
        dst: d,
        src,
        from,
        to,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
}

/// Packed float↔int convert `cvt*p*` (task-239): `dst = op0`, source = `op1` (xmm or
/// memory). A memory source is materialised into `dst` first (`VLoad`, sized per `kind`),
/// then converted in place — the `pshufd` pattern. `vex` appends the VEX.128 upper-zeroing
/// (`Vcvt*` mnemonics). YMM/EVEX (256/512-bit) forms make `reg_xmm` `None` → unsupported
/// (deferred); only the 128-bit forms lift.
pub(crate) fn lift_packed_cvt(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    kind: PackedCvtKind,
    vex: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let src = match reg_xmm(insn, 1) {
        Some(a) => a,
        None if insn.op_kind(1) == OpKind::Memory => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::VLoad {
                dst: d,
                addr,
                size: kind.mem_bytes(),
            });
            d
        }
        None => return Err(unsupported_insn(insn)),
    };
    ops.push(IrOp::VPackedCvt { dst: d, src, kind });
    if vex {
        ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    }
    Ok(())
}
