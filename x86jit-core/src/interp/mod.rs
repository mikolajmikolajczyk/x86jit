//! IR interpreter (§8.1). Walks an `IrBlock`'s ops over a `temps` vector and a
//! `&mut CpuState`, reading/writing shared guest `&Memory`. Slow but simple — the
//! oracle the JIT is validated against.
//!
//! RIP-on-trap convention (§8, §16), identical to the JIT's: on a memory trap RIP
//! is set to the FAULTING instruction (`cur_addr`, from `InsnStart`) so the user
//! can map/handle and retry; after `syscall`/`hlt` RIP is PAST the instruction.

use crate::exit::{AccessKind, Exit, PortDir, StepResult};
use std::cmp::Ordering;

use crate::ir::{
    Cond, FPrec, FlagMask, FloatBinOp, FloatUnOp, IrBlock, IrOp, PackedBinOp, RepKind, StrOp,
    VLogicOp, Val,
};
use crate::memory::{MemTrap, Memory};
use crate::state::{CpuState, Flags, Reg};

/// `gpr[]` slot for RSP (used by push/pop-style stack ops in Call/Ret).
const RSP: usize = 4;

/// Single-step the interpreter over exactly one instruction at `cpu.rip` (§5.2,
/// M4-T10). The dispatcher calls this to service an MMIO access the JIT deferred:
/// the interpreter re-executes the faulting instruction, which either traps out
/// (`MmioRead`/`MmioWrite`) or — on resume, once the embedder supplied the value
/// via `complete_mmio_read` / acknowledged the write via `complete_mmio_write` —
/// consumes it and advances RIP. A lift/decode error becomes the matching exit.
pub fn step_one(
    mem: &Memory,
    cpu: &mut CpuState,
    mode: crate::lift::CpuMode,
    scratch: &mut Vec<u64>,
) -> StepResult {
    match crate::lift::lift_one(mem, cpu.rip, mode) {
        Ok(ir) => interpret_block(&ir, cpu, mem, scratch),
        Err(crate::lift::LiftError::Unsupported { addr, bytes, len }) => {
            StepResult::Exit(Exit::UnknownInstruction { addr, bytes, len })
        }
        Err(crate::lift::LiftError::DecodeFault { addr }) => {
            StepResult::Exit(Exit::UnmappedMemory {
                addr,
                access: AccessKind::Execute,
            })
        }
    }
}

pub fn interpret_block(
    ir: &IrBlock,
    cpu: &mut CpuState,
    mem: &Memory,
    scratch: &mut Vec<u64>,
) -> StepResult {
    // Reuse the caller's scratch buffer across blocks instead of allocating a fresh
    // temps vector every dispatch (hot path). `clear` + `resize(_, 0)` keeps the
    // allocation and zero-fills all slots.
    scratch.clear();
    scratch.resize(ir.temp_count as usize, 0);
    let temps: &mut [u64] = scratch;
    let mut cur_addr = ir.guest_start;
    let mut bracket = crate::lockstep::begin();

    for op in &ir.ops {
        match op {
            IrOp::InsnStart { guest_addr } => {
                cur_addr = *guest_addr;
                if bracket.active() {
                    crate::lockstep::on_insn_start(&mut bracket, cpu, mem, *guest_addr);
                }
            }
            IrOp::ReadReg { dst, reg } => {
                if let Some(r) = exec_read_reg(cpu, temps, dst, reg) {
                    return r;
                }
            }
            IrOp::WriteReg { reg, src, size } => {
                if let Some(r) = exec_write_reg(cpu, temps, reg, src, size) {
                    return r;
                }
            }
            IrOp::Add {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_add(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Adc {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_adc(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Sub {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_sub(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Sbb {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_sbb(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::And {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_and(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Or {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_or(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Xor {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_xor(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Shl {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_shl(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Shr {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_shr(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::DoubleShift {
                dst,
                a,
                b,
                count,
                size,
                left,
                set_flags,
            } => {
                if let Some(r) =
                    exec_double_shift(cpu, temps, dst, a, b, count, size, left, set_flags)
                {
                    return r;
                }
            }
            IrOp::Sar {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_sar(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Sext { dst, a, from } => {
                if let Some(r) = exec_sext(temps, dst, a, from) {
                    return r;
                }
            }
            IrOp::Bswap { dst, a, size } => {
                if let Some(r) = exec_bswap(temps, dst, a, size) {
                    return r;
                }
            }
            IrOp::Rol {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_rol(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Ror {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_ror(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Rcl {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_rcl(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Rcr {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                if let Some(r) = exec_rcr(cpu, temps, dst, a, b, size, set_flags) {
                    return r;
                }
            }
            IrOp::Mul {
                lo,
                hi,
                a,
                b,
                size,
                signed,
                set_flags,
            } => {
                if let Some(r) = exec_mul(cpu, temps, lo, hi, a, b, size, signed, set_flags) {
                    return r;
                }
            }
            IrOp::Div {
                quot,
                rem,
                hi,
                lo,
                divisor,
                size,
                signed,
            } => {
                if let Some(r) = exec_div(
                    cpu, temps, cur_addr, quot, rem, hi, lo, divisor, size, signed,
                ) {
                    return r;
                }
            }
            IrOp::GetCond { dst, cond } => {
                if let Some(r) = exec_get_cond(cpu, temps, dst, cond) {
                    return r;
                }
            }
            IrOp::Load { dst, addr, size } => {
                if let Some(r) = exec_load(cpu, mem, temps, cur_addr, dst, addr, size) {
                    return r;
                }
            }
            IrOp::Store {
                addr, src, size, ..
            } => {
                if let Some(r) = exec_store(cpu, mem, temps, cur_addr, addr, src, size) {
                    return r;
                }
            }
            IrOp::AtomicRmw {
                old,
                addr,
                src,
                size,
                op,
            } => {
                if let Some(r) =
                    exec_atomic_rmw(cpu, mem, temps, cur_addr, old, addr, src, size, op)
                {
                    return r;
                }
            }
            IrOp::AtomicCas {
                old,
                addr,
                expected,
                src,
                size,
            } => {
                if let Some(r) =
                    exec_atomic_cas(cpu, mem, temps, cur_addr, old, addr, expected, src, size)
                {
                    return r;
                }
            }
            IrOp::Bt {
                result,
                a,
                bit,
                size,
                op,
            } => {
                if let Some(r) = exec_bt(cpu, temps, result, a, bit, size, op) {
                    return r;
                }
            }
            IrOp::Cpuid => {
                if let Some(r) = exec_cpuid(cpu) {
                    return r;
                }
            }
            IrOp::Xgetbv => {
                if let Some(r) = exec_xgetbv(cpu) {
                    return r;
                }
            }
            IrOp::X87 { kind, addr, sti } => {
                if let Some(r) = exec_x87(cpu, mem, temps, cur_addr, kind, addr, sti) {
                    return r;
                }
            }
            IrOp::FxState { addr, restore } => {
                if let Some(r) = exec_fx_state(cpu, mem, temps, cur_addr, addr, restore) {
                    return r;
                }
            }
            IrOp::Popcnt { dst, src, size } => {
                if let Some(r) = exec_popcnt(cpu, temps, dst, src, size) {
                    return r;
                }
            }
            IrOp::Crc32 {
                dst,
                crc,
                src,
                bytes,
            } => {
                if let Some(r) = exec_crc32(temps, dst, crc, src, bytes) {
                    return r;
                }
            }
            IrOp::Bmi {
                dst,
                a,
                b,
                size,
                op,
            } => {
                if let Some(r) = exec_bmi(cpu, temps, dst, a, b, size, op) {
                    return r;
                }
            }
            IrOp::BitScan {
                dst,
                src,
                old,
                size,
                op,
            } => {
                if let Some(r) = exec_bit_scan(cpu, temps, dst, src, old, size, op) {
                    return r;
                }
            }
            IrOp::VLoad { dst, addr, size } => {
                if let Some(r) = exec_v_load(cpu, mem, temps, cur_addr, dst, addr, size) {
                    return r;
                }
            }
            IrOp::VStore { addr, src, size } => {
                if let Some(r) = exec_v_store(cpu, mem, temps, cur_addr, addr, src, size) {
                    return r;
                }
            }
            IrOp::VMov { dst, src } => {
                if let Some(r) = exec_v_mov(cpu, dst, src) {
                    return r;
                }
            }
            IrOp::VLoadWide { dst, addr, bytes } => {
                if let Some(r) = exec_v_load_wide(cpu, mem, temps, cur_addr, dst, addr, bytes) {
                    return r;
                }
            }
            IrOp::VStoreWide { addr, src, bytes } => {
                if let Some(r) = exec_v_store_wide(cpu, mem, temps, cur_addr, addr, src, bytes) {
                    return r;
                }
            }
            IrOp::VMovWide { dst, src, bytes } => {
                if let Some(r) = exec_v_mov_wide(cpu, dst, src, bytes) {
                    return r;
                }
            }
            IrOp::VMaskMov {
                dst,
                src,
                k,
                elem,
                zeroing,
                bytes,
            } => {
                if let Some(r) = exec_v_mask_mov(cpu, dst, src, k, elem, zeroing, bytes) {
                    return r;
                }
            }
            IrOp::VMaskLoadMem {
                dst,
                addr,
                k,
                elem,
                zeroing,
                bytes,
            } => {
                if let Some(r) = exec_v_mask_load_mem(
                    cpu, mem, temps, cur_addr, dst, addr, k, elem, zeroing, bytes,
                ) {
                    return r;
                }
            }
            IrOp::VMaskStoreMem {
                src,
                addr,
                k,
                elem,
                bytes,
            } => {
                if let Some(r) =
                    exec_v_mask_store_mem(cpu, mem, temps, cur_addr, src, addr, k, elem, bytes)
                {
                    return r;
                }
            }
            IrOp::VLogic256 { dst, a, b, op } => {
                if let Some(r) = exec_v_logic256(cpu, dst, a, b, op) {
                    return r;
                }
            }
            IrOp::VLogicWide {
                dst,
                a,
                b,
                op,
                bytes,
            } => {
                if let Some(r) = exec_v_logic_wide(cpu, dst, a, b, op, bytes) {
                    return r;
                }
            }
            IrOp::VLogicWideM {
                dst,
                a,
                addr,
                op,
                bytes,
            } => {
                if let Some(r) =
                    exec_v_logic_wide_m(cpu, mem, temps, cur_addr, dst, a, addr, op, bytes)
                {
                    return r;
                }
            }
            IrOp::VPopcnt {
                dst,
                a,
                lane,
                bytes,
            } => {
                if let Some(r) = exec_v_popcnt(cpu, dst, a, lane, bytes) {
                    return r;
                }
            }
            IrOp::VPopcntM {
                dst,
                addr,
                lane,
                bytes,
            } => {
                if let Some(r) = exec_v_popcnt_m(cpu, mem, temps, cur_addr, dst, addr, lane, bytes)
                {
                    return r;
                }
            }
            IrOp::VPMovExtend {
                dst,
                src,
                from,
                to,
                signed,
            } => {
                if let Some(r) = exec_v_p_mov_extend(cpu, dst, src, from, to, signed) {
                    return r;
                }
            }
            IrOp::VPMovExtendM {
                dst,
                addr,
                from,
                to,
                signed,
            } => {
                if let Some(r) =
                    exec_v_p_mov_extend_m(cpu, mem, temps, cur_addr, dst, addr, from, to, signed)
                {
                    return r;
                }
            }
            IrOp::VPMovExtendWide {
                dst,
                src,
                from,
                to,
                signed,
                dst_width,
                writemask,
                zeroing,
            } => {
                if let Some(r) = exec_v_p_mov_extend_wide(
                    cpu, dst, src, from, to, signed, dst_width, writemask, zeroing,
                ) {
                    return r;
                }
            }
            IrOp::VPAbs {
                dst,
                src,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => {
                if let Some(r) = exec_v_p_abs(cpu, dst, src, elem, dst_width, writemask, zeroing) {
                    return r;
                }
            }
            IrOp::VpUnaryLane {
                dst,
                src,
                op,
                imm,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => {
                if let Some(r) =
                    exec_v_p_unary_lane(cpu, dst, src, op, imm, elem, dst_width, writemask, zeroing)
                {
                    return r;
                }
            }
            IrOp::VpBlendm {
                dst,
                a,
                b,
                k,
                elem,
                dst_width,
                zeroing,
            } => {
                if let Some(r) = exec_v_p_blendm(cpu, dst, a, b, k, elem, dst_width, zeroing) {
                    return r;
                }
            }
            IrOp::VShuffLane {
                dst,
                a,
                b,
                imm,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => {
                if let Some(r) =
                    exec_v_shuf_lane(cpu, dst, a, b, imm, elem, dst_width, writemask, zeroing)
                {
                    return r;
                }
            }
            IrOp::VpMultishift {
                dst,
                ctrl,
                data,
                dst_width,
                writemask,
                zeroing,
            } => {
                if let Some(r) =
                    exec_v_p_multishift(cpu, dst, ctrl, data, dst_width, writemask, zeroing)
                {
                    return r;
                }
            }
            IrOp::VPBlendV { dst, src, lane } => {
                if let Some(r) = exec_v_p_blend_v(cpu, dst, src, lane) {
                    return r;
                }
            }
            IrOp::VPBlendVM { dst, addr, lane } => {
                if let Some(r) = exec_v_p_blend_v_m(cpu, mem, temps, cur_addr, dst, addr, lane) {
                    return r;
                }
            }
            IrOp::VPBlendVX {
                dst,
                a,
                b,
                mask,
                lane,
            } => {
                if let Some(r) = exec_v_p_blend_v_x(cpu, dst, a, b, mask, lane) {
                    return r;
                }
            }
            IrOp::VPRound {
                dst,
                a,
                src,
                prec,
                mode,
                scalar,
            } => {
                if let Some(r) = exec_v_p_round(cpu, dst, a, src, prec, mode, scalar) {
                    return r;
                }
            }
            IrOp::VPRoundM {
                dst,
                addr,
                prec,
                mode,
                scalar,
            } => {
                if let Some(r) =
                    exec_v_p_round_m(cpu, mem, temps, cur_addr, dst, addr, prec, mode, scalar)
                {
                    return r;
                }
            }
            IrOp::VMaskedLogic {
                dst,
                a,
                b,
                op,
                k,
                elem,
                zeroing,
                bytes,
            } => {
                if let Some(r) = exec_v_masked_logic(cpu, dst, a, b, op, k, elem, zeroing, bytes) {
                    return r;
                }
            }
            IrOp::VMaskedPacked {
                dst,
                a,
                b,
                op,
                k,
                elem,
                zeroing,
                bytes,
            } => {
                if let Some(r) = exec_v_masked_packed(cpu, dst, a, b, op, k, elem, zeroing, bytes) {
                    return r;
                }
            }
            IrOp::VInsertLaneWide {
                dst,
                src,
                ins,
                idx,
                num_lanes,
                bytes,
            } => {
                if let Some(r) = exec_v_insert_lane_wide(cpu, dst, src, ins, idx, num_lanes, bytes)
                {
                    return r;
                }
            }
            IrOp::VExtractLaneWide {
                dst,
                src,
                idx,
                num_lanes,
            } => {
                if let Some(r) = exec_v_extract_lane_wide(cpu, dst, src, idx, num_lanes) {
                    return r;
                }
            }
            IrOp::VExtractLaneWideM {
                src,
                addr,
                idx,
                num_lanes,
            } => {
                if let Some(r) =
                    exec_v_extract_lane_wide_m(cpu, mem, temps, cur_addr, src, addr, idx, num_lanes)
                {
                    return r;
                }
            }
            IrOp::VPcmpStr {
                a,
                b,
                imm,
                explicit,
            } => {
                if let Some(r) = exec_v_pcmp_str(cpu, a, b, imm, explicit) {
                    return r;
                }
            }
            IrOp::VPcmpStrM {
                a,
                addr,
                imm,
                explicit,
            } => {
                if let Some(r) =
                    exec_v_pcmp_str_m(cpu, mem, temps, cur_addr, a, addr, imm, explicit)
                {
                    return r;
                }
            }
            IrOp::VAlign {
                dst,
                a,
                b,
                shift,
                elem,
                bytes,
            } => {
                if let Some(r) = exec_v_align(cpu, dst, a, b, shift, elem, bytes) {
                    return r;
                }
            }
            IrOp::VPTernlog {
                dst,
                b,
                c,
                imm,
                bytes,
            } => {
                if let Some(r) = exec_v_p_ternlog(cpu, dst, b, c, imm, bytes) {
                    return r;
                }
            }
            IrOp::VPTernlogM {
                dst,
                b,
                addr,
                imm,
                bytes,
            } => {
                if let Some(r) =
                    exec_v_p_ternlog_m(cpu, mem, temps, cur_addr, dst, b, addr, imm, bytes)
                {
                    return r;
                }
            }
            IrOp::VLogic256M { dst, a, addr, op } => {
                if let Some(r) = exec_v_logic256_m(cpu, mem, temps, cur_addr, dst, a, addr, op) {
                    return r;
                }
            }
            IrOp::VPackedBin256 {
                dst,
                a,
                b,
                lane,
                op,
            } => {
                if let Some(r) = exec_v_packed_bin256(cpu, dst, a, b, lane, op) {
                    return r;
                }
            }
            IrOp::VPackedBin256M {
                dst,
                a,
                addr,
                lane,
                op,
            } => {
                if let Some(r) =
                    exec_v_packed_bin256_m(cpu, mem, temps, cur_addr, dst, a, addr, lane, op)
                {
                    return r;
                }
            }
            IrOp::VPackedWide {
                dst,
                a,
                b,
                lane,
                op,
                bytes,
            } => {
                if let Some(r) = exec_v_packed_wide(cpu, dst, a, b, lane, op, bytes) {
                    return r;
                }
            }
            IrOp::VPackedWideM {
                dst,
                a,
                addr,
                lane,
                op,
                bytes,
            } => {
                if let Some(r) =
                    exec_v_packed_wide_m(cpu, mem, temps, cur_addr, dst, a, addr, lane, op, bytes)
                {
                    return r;
                }
            }
            IrOp::VMoveMaskB256 { dst, src } => {
                if let Some(r) = exec_v_move_mask_b256(cpu, temps, dst, src) {
                    return r;
                }
            }
            IrOp::VFromGpr { dst, src, size } => {
                if let Some(r) = exec_v_from_gpr(cpu, temps, dst, src, size) {
                    return r;
                }
            }
            IrOp::VToGpr { dst, src, size } => {
                if let Some(r) = exec_v_to_gpr(cpu, temps, dst, src, size) {
                    return r;
                }
            }
            IrOp::VLogic { dst, a, b, op } => {
                if let Some(r) = exec_v_logic(cpu, dst, a, b, op) {
                    return r;
                }
            }
            IrOp::VPackedBin {
                dst,
                a,
                b,
                lane,
                op,
            } => {
                if let Some(r) = exec_v_packed_bin(cpu, dst, a, b, lane, op) {
                    return r;
                }
            }
            IrOp::VPackedBinM {
                dst,
                addr,
                lane,
                op,
            } => {
                if let Some(r) = exec_v_packed_bin_m(cpu, mem, temps, cur_addr, dst, addr, lane, op)
                {
                    return r;
                }
            }
            IrOp::VLogicM { dst, addr, op } => {
                if let Some(r) = exec_v_logic_m(cpu, mem, temps, cur_addr, dst, addr, op) {
                    return r;
                }
            }
            IrOp::VPackedShift {
                dst,
                a,
                imm,
                lane,
                right,
                arith,
            } => {
                if let Some(r) = exec_v_packed_shift(cpu, dst, a, imm, lane, right, arith) {
                    return r;
                }
            }
            IrOp::VMaskedShift {
                dst,
                a,
                imm,
                elem,
                right,
                arith,
                k,
                zeroing,
                bytes,
            } => {
                if let Some(r) =
                    exec_v_masked_shift(cpu, dst, a, imm, elem, right, arith, k, zeroing, bytes)
                {
                    return r;
                }
            }
            IrOp::VShiftReg {
                dst,
                a,
                count,
                elem,
                right,
                arith,
                k,
                zeroing,
                bytes,
            } => {
                exec_shift_reg(
                    cpu, *dst, *a, *count, *elem, *right, *arith, *k, *zeroing, *bytes,
                );
            }
            IrOp::VShiftVar {
                dst,
                a,
                count,
                elem,
                right,
                arith,
                k,
                zeroing,
                bytes,
            } => {
                exec_var_shift(
                    cpu, *dst, *a, *count, *elem, *right, *arith, *k, *zeroing, *bytes,
                );
            }
            IrOp::VGf2p8 {
                dst,
                a,
                b,
                imm,
                mode,
                k,
                zeroing,
                bytes,
            } => {
                exec_gf2p8(cpu, *dst, *a, *b, *imm, *mode, *k, *zeroing, *bytes);
            }
            IrOp::VGf2p8M {
                dst,
                a,
                addr,
                imm,
                mode,
                k,
                zeroing,
                bytes,
            } => {
                if let Some(r) =
                    exec_v_gf2p8_m(cpu, mem, temps, dst, a, addr, imm, mode, k, zeroing, bytes)
                {
                    return r;
                }
            }
            IrOp::VByteShift {
                dst,
                a,
                bytes,
                right,
            } => {
                if let Some(r) = exec_v_byte_shift(cpu, dst, a, bytes, right) {
                    return r;
                }
            }
            IrOp::VShuffle32 { dst, a, imm } => {
                if let Some(r) = exec_v_shuffle32(cpu, dst, a, imm) {
                    return r;
                }
            }
            IrOp::VBlendW { dst, a, b, imm } => {
                if let Some(r) = exec_v_blend_w(cpu, dst, a, b, imm) {
                    return r;
                }
            }
            IrOp::VBlendD {
                dst,
                a,
                b,
                imm,
                bytes,
            } => {
                if let Some(r) = exec_v_blend_d(cpu, dst, a, b, imm, bytes) {
                    return r;
                }
            }
            IrOp::VFma {
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
            } => {
                if let Some(r) = exec_v_fma(
                    cpu, dst, x, y, z, prec, scalar, neg_prod, neg_add, bytes, writemask, zeroing,
                ) {
                    return r;
                }
            }
            IrOp::VFmaM {
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
            } => {
                if let Some(r) = exec_v_fma_m(
                    cpu, mem, temps, cur_addr, dst, x, y, z, addr, mem_role, prec, scalar,
                    neg_prod, neg_add, bytes, writemask, zeroing,
                ) {
                    return r;
                }
            }
            IrOp::VPackWide {
                dst,
                a,
                b,
                from_elem,
                signed,
                bytes,
            } => {
                if let Some(r) = exec_v_pack_wide(cpu, dst, a, b, from_elem, signed, bytes) {
                    return r;
                }
            }
            IrOp::VShuffle32Wide {
                dst,
                a,
                imm,
                bytes,
                writemask,
                zeroing,
            } => {
                if let Some(r) = exec_v_shuffle32_wide(cpu, dst, a, imm, bytes, writemask, zeroing)
                {
                    return r;
                }
            }
            IrOp::VMoveHalf {
                dst,
                src,
                dst_high,
                src_high,
            } => {
                if let Some(r) = exec_v_move_half(cpu, dst, src, dst_high, src_high) {
                    return r;
                }
            }
            IrOp::VLoadHalf { dst, addr, high } => {
                if let Some(r) = exec_v_load_half(cpu, mem, temps, cur_addr, dst, addr, high) {
                    return r;
                }
            }
            IrOp::VStoreHalf { addr, src, high } => {
                if let Some(r) = exec_v_store_half(cpu, mem, temps, cur_addr, addr, src, high) {
                    return r;
                }
            }
            IrOp::VExtractW { dst, src, index } => {
                if let Some(r) = exec_v_extract_w(cpu, temps, dst, src, index) {
                    return r;
                }
            }
            IrOp::VExtractLane {
                dst,
                src,
                index,
                size,
            } => {
                if let Some(r) = exec_v_extract_lane(cpu, temps, dst, src, index, size) {
                    return r;
                }
            }
            IrOp::VMoveMaskB { dst, src } => {
                if let Some(r) = exec_v_move_mask_b(cpu, temps, dst, src) {
                    return r;
                }
            }
            IrOp::VBroadcast {
                dst,
                src,
                elem,
                w256,
            } => {
                if let Some(r) = exec_v_broadcast(cpu, dst, src, elem, w256) {
                    return r;
                }
            }
            IrOp::VBroadcastM {
                dst,
                addr,
                elem,
                w256,
            } => {
                if let Some(r) =
                    exec_v_broadcast_m(cpu, mem, temps, cur_addr, dst, addr, elem, w256)
                {
                    return r;
                }
            }
            IrOp::VBroadcastGpr {
                dst,
                src,
                elem,
                width,
            } => {
                if let Some(r) = exec_v_broadcast_gpr(cpu, temps, dst, src, elem, width) {
                    return r;
                }
            }
            IrOp::VBroadcastLane {
                dst,
                src,
                chunk,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => {
                if let Some(r) =
                    exec_v_broadcast_lane(cpu, dst, src, chunk, elem, dst_width, writemask, zeroing)
                {
                    return r;
                }
            }
            IrOp::VBroadcastLaneM {
                dst,
                addr,
                chunk,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => {
                if let Some(r) = exec_v_broadcast_lane_m(
                    cpu, mem, temps, cur_addr, dst, addr, chunk, elem, dst_width, writemask,
                    zeroing,
                ) {
                    return r;
                }
            }
            IrOp::VPCmpToMask {
                k,
                a,
                b,
                elem,
                width,
                pred,
                signed,
                writemask,
            } => {
                if let Some(r) =
                    exec_v_p_cmp_to_mask(cpu, k, a, b, elem, width, pred, signed, writemask)
                {
                    return r;
                }
            }
            IrOp::VPCmpToMaskM {
                k,
                a,
                addr,
                elem,
                width,
                pred,
                signed,
                writemask,
            } => {
                if let Some(r) = exec_v_p_cmp_to_mask_m(
                    cpu, mem, temps, cur_addr, k, a, addr, elem, width, pred, signed, writemask,
                ) {
                    return r;
                }
            }
            IrOp::VPTestToMask {
                k,
                a,
                b,
                elem,
                width,
                neg,
                writemask,
            } => {
                if let Some(r) = exec_v_p_test_to_mask(cpu, k, a, b, elem, width, neg, writemask) {
                    return r;
                }
            }
            IrOp::VPTestToMaskM {
                k,
                a,
                addr,
                elem,
                width,
                neg,
                writemask,
            } => {
                if let Some(r) = exec_v_p_test_to_mask_m(
                    cpu, mem, temps, cur_addr, k, a, addr, elem, width, neg, writemask,
                ) {
                    return r;
                }
            }
            IrOp::VKOrTest { a, b, width } => {
                if let Some(r) = exec_v_k_or_test(cpu, a, b, width) {
                    return r;
                }
            }
            IrOp::VKFromGpr { k, src, width } => {
                if let Some(r) = exec_v_k_from_gpr(cpu, temps, k, src, width) {
                    return r;
                }
            }
            IrOp::VKToGpr { dst, k, width } => {
                if let Some(r) = exec_v_k_to_gpr(cpu, temps, dst, k, width) {
                    return r;
                }
            }
            IrOp::VKMovKK { dst, src, width } => {
                if let Some(r) = exec_v_k_mov_k_k(cpu, dst, src, width) {
                    return r;
                }
            }
            IrOp::VKUnpack { dst, a, b, half } => {
                if let Some(r) = exec_v_k_unpack(cpu, dst, a, b, half) {
                    return r;
                }
            }
            IrOp::VKBinOp {
                dst,
                a,
                b,
                op,
                width,
            } => {
                if let Some(r) = exec_v_k_bin_op(cpu, dst, a, b, op, width) {
                    return r;
                }
            }
            IrOp::VKNot { dst, a, width } => {
                if let Some(r) = exec_v_k_not(cpu, dst, a, width) {
                    return r;
                }
            }
            IrOp::VKShift {
                dst,
                a,
                amount,
                width,
                left,
            } => {
                if let Some(r) = exec_v_k_shift(cpu, dst, a, amount, width, left) {
                    return r;
                }
            }
            IrOp::VPmovNarrow {
                dst,
                src,
                from,
                to,
                src_width,
                writemask,
                zeroing,
            } => {
                if let Some(r) =
                    exec_v_pmov_narrow(cpu, dst, src, from, to, src_width, writemask, zeroing)
                {
                    return r;
                }
            }
            IrOp::VPmovNarrowMem {
                src,
                addr,
                from,
                to,
                src_width,
            } => {
                if let Some(r) = exec_v_pmov_narrow_mem(
                    cpu, mem, temps, cur_addr, src, addr, from, to, src_width,
                ) {
                    return r;
                }
            }
            IrOp::VPermT2 {
                dst,
                idx,
                tbl,
                elem,
                writemask,
                zeroing,
                bytes,
                imode,
            } => {
                if let Some(r) =
                    exec_v_perm_t2(cpu, dst, idx, tbl, elem, writemask, zeroing, bytes, imode)
                {
                    return r;
                }
            }
            IrOp::VPermT2M {
                dst,
                idx,
                addr,
                elem,
                writemask,
                zeroing,
                bytes,
                imode,
            } => {
                if let Some(r) = exec_v_perm_t2_m(
                    cpu, mem, temps, cur_addr, dst, idx, addr, elem, writemask, zeroing, bytes,
                    imode,
                ) {
                    return r;
                }
            }
            IrOp::VPerm1 {
                dst,
                idx,
                src,
                elem,
                bytes,
                writemask,
                zeroing,
            } => {
                if let Some(r) = exec_v_perm1(cpu, dst, idx, src, elem, bytes, writemask, zeroing) {
                    return r;
                }
            }
            IrOp::VPerm1M {
                dst,
                idx,
                addr,
                elem,
                bytes,
                writemask,
                zeroing,
            } => {
                if let Some(r) = exec_v_perm1_m(
                    cpu, mem, temps, cur_addr, dst, idx, addr, elem, bytes, writemask, zeroing,
                ) {
                    return r;
                }
            }
            IrOp::VInsert128 { dst, src, ins, hi } => {
                if let Some(r) = exec_v_insert128(cpu, dst, src, ins, hi) {
                    return r;
                }
            }
            IrOp::VInsert128M { dst, src, addr, hi } => {
                if let Some(r) = exec_v_insert128_m(cpu, mem, temps, cur_addr, dst, src, addr, hi) {
                    return r;
                }
            }
            IrOp::VExtract128 { dst, src, hi } => {
                if let Some(r) = exec_v_extract128(cpu, dst, src, hi) {
                    return r;
                }
            }
            IrOp::VPshufb256 { dst, a, idx } => {
                if let Some(r) = exec_v_pshufb256(cpu, dst, a, idx) {
                    return r;
                }
            }
            IrOp::VPshufbWide {
                dst,
                a,
                idx,
                bytes,
                writemask,
                zeroing,
            } => {
                if let Some(r) = exec_v_pshufb_wide(cpu, dst, a, idx, bytes, writemask, zeroing) {
                    return r;
                }
            }
            IrOp::VPshufb256M { dst, a, addr } => {
                if let Some(r) = exec_v_pshufb256_m(cpu, mem, temps, cur_addr, dst, a, addr) {
                    return r;
                }
            }
            IrOp::VPackedShift256 {
                dst,
                a,
                imm,
                lane,
                right,
                arith,
            } => {
                if let Some(r) = exec_v_packed_shift256(cpu, dst, a, imm, lane, right, arith) {
                    return r;
                }
            }
            IrOp::VPermq { dst, src, imm } => {
                if let Some(r) = exec_v_permq(cpu, dst, src, imm) {
                    return r;
                }
            }
            IrOp::VPermd { dst, ctrl, src } => {
                if let Some(r) = exec_v_permd(cpu, dst, ctrl, src) {
                    return r;
                }
            }
            IrOp::VPerm2i128 { dst, a, b, imm } => {
                if let Some(r) = exec_v_perm2i128(cpu, dst, a, b, imm) {
                    return r;
                }
            }
            IrOp::VPalignr256 { dst, a, b, imm } => {
                if let Some(r) = exec_v_palignr256(cpu, dst, a, b, imm) {
                    return r;
                }
            }
            IrOp::VPtest { a, b, w256 } => {
                if let Some(r) = exec_v_ptest(cpu, a, b, w256) {
                    return r;
                }
            }
            IrOp::VZeroUpper { reg } => {
                if let Some(r) = exec_v_zero_upper(cpu, reg) {
                    return r;
                }
            }
            IrOp::VZeroUpperAll { clear_low } => {
                if let Some(r) = exec_v_zero_upper_all(cpu, *clear_low) {
                    return r;
                }
            }
            IrOp::VPshufb { dst, a, idx } => {
                if let Some(r) = exec_v_pshufb(cpu, dst, a, idx) {
                    return r;
                }
            }
            IrOp::VPshufbM { dst, addr } => {
                if let Some(r) = exec_v_pshufb_m(cpu, mem, temps, cur_addr, dst, addr) {
                    return r;
                }
            }
            IrOp::VAlignr { dst, a, src, imm } => {
                if let Some(r) = exec_v_alignr(cpu, dst, a, src, imm) {
                    return r;
                }
            }
            IrOp::VAlignrM { dst, addr, imm } => {
                if let Some(r) = exec_v_alignr_m(cpu, mem, temps, cur_addr, dst, addr, imm) {
                    return r;
                }
            }
            IrOp::VAes { dst, a, b, op } => {
                if let Some(r) = exec_v_aes(cpu, dst, a, b, op) {
                    return r;
                }
            }
            IrOp::VAesM { dst, a, addr, op } => {
                if let Some(r) = exec_v_aes_m(cpu, mem, temps, cur_addr, dst, a, addr, op) {
                    return r;
                }
            }
            IrOp::VAesImc { dst, src } => {
                if let Some(r) = exec_v_aes_imc(cpu, dst, src) {
                    return r;
                }
            }
            IrOp::VAesImcM { dst, addr } => {
                if let Some(r) = exec_v_aes_imc_m(cpu, mem, temps, cur_addr, dst, addr) {
                    return r;
                }
            }
            IrOp::VAesKeygen { dst, src, imm } => {
                if let Some(r) = exec_v_aes_keygen(cpu, dst, src, imm) {
                    return r;
                }
            }
            IrOp::VAesKeygenM { dst, addr, imm } => {
                if let Some(r) = exec_v_aes_keygen_m(cpu, mem, temps, cur_addr, dst, addr, imm) {
                    return r;
                }
            }
            IrOp::VSha { dst, a, b, imm, op } => {
                if let Some(r) = exec_v_sha(cpu, dst, a, b, imm, op) {
                    return r;
                }
            }
            IrOp::VShaM {
                dst,
                a,
                addr,
                imm,
                op,
            } => {
                if let Some(r) = exec_v_sha_m(cpu, mem, temps, cur_addr, dst, a, addr, imm, op) {
                    return r;
                }
            }
            IrOp::VGfni { dst, a, b, imm, op } => {
                if let Some(r) = exec_v_gfni(cpu, dst, a, b, imm, op) {
                    return r;
                }
            }
            IrOp::VGfniM {
                dst,
                a,
                addr,
                imm,
                op,
            } => {
                if let Some(r) = exec_v_gfni_m(cpu, mem, temps, cur_addr, dst, a, addr, imm, op) {
                    return r;
                }
            }
            IrOp::Movq2dq { dst, src_mm } => exec_movq2dq(cpu, *dst, *src_mm),
            IrOp::Movdq2q { dst_mm, src_xmm } => exec_movdq2q(cpu, *dst_mm, *src_xmm),
            IrOp::VPclmul { dst, a, b, imm } => {
                if let Some(r) = exec_v_pclmul(cpu, dst, a, b, imm) {
                    return r;
                }
            }
            IrOp::VPclmulM { dst, a, addr, imm } => {
                if let Some(r) = exec_v_pclmul_m(cpu, mem, temps, cur_addr, dst, a, addr, imm) {
                    return r;
                }
            }
            IrOp::VPsign { dst, a, b, lane } => {
                if let Some(r) = exec_v_psign(cpu, dst, a, b, lane) {
                    return r;
                }
            }
            IrOp::VPsignM { dst, a, addr, lane } => {
                if let Some(r) = exec_v_psign_m(cpu, mem, temps, cur_addr, dst, a, addr, lane) {
                    return r;
                }
            }
            IrOp::VShufps { dst, a, b, imm } => {
                if let Some(r) = exec_v_shufps(cpu, dst, a, b, imm) {
                    return r;
                }
            }
            IrOp::VShuffle16 { dst, a, imm, high } => {
                if let Some(r) = exec_v_shuffle16(cpu, dst, a, imm, high) {
                    return r;
                }
            }
            IrOp::VUnpackLow {
                dst,
                a,
                b,
                lane,
                high,
            } => {
                if let Some(r) = exec_v_unpack_low(cpu, dst, a, b, lane, high) {
                    return r;
                }
            }
            IrOp::VPackUsWB { dst, a, b } => {
                if let Some(r) = exec_v_pack_us_w_b(cpu, dst, a, b) {
                    return r;
                }
            }
            IrOp::SetDf { value } => {
                if let Some(r) = exec_set_df(cpu, value) {
                    return r;
                }
            }
            IrOp::RepString { op, elem, rep } => {
                if let Some(r) = exec_rep_string(cpu, mem, cur_addr, op, elem, rep) {
                    return r;
                }
            }
            IrOp::VInsertW { dst, src, index } => {
                if let Some(r) = exec_v_insert_w(cpu, temps, dst, src, index) {
                    return r;
                }
            }
            IrOp::VInsertLane {
                dst,
                base,
                src,
                index,
                size,
            } => {
                if let Some(r) = exec_v_insert_lane(cpu, temps, dst, base, src, index, size) {
                    return r;
                }
            }
            IrOp::VFloatMov { dst, a, src, prec } => {
                if let Some(r) = exec_v_float_mov(cpu, dst, a, src, prec) {
                    return r;
                }
            }
            IrOp::VFloatBin {
                dst,
                a,
                b,
                op,
                prec,
                scalar,
            } => {
                if let Some(r) = exec_v_float_bin(cpu, dst, a, b, op, prec, scalar) {
                    return r;
                }
            }
            IrOp::VFloatBinM {
                dst,
                addr,
                op,
                prec,
                scalar,
            } => {
                if let Some(r) =
                    exec_v_float_bin_m(cpu, mem, temps, cur_addr, dst, addr, op, prec, scalar)
                {
                    return r;
                }
            }
            IrOp::VFloatCmpMask {
                dst,
                a,
                b,
                prec,
                scalar,
                pred,
            } => {
                if let Some(r) = exec_v_float_cmp_mask(cpu, dst, a, b, prec, scalar, pred) {
                    return r;
                }
            }
            IrOp::VFloatCmp { a, b, prec } => {
                if let Some(r) = exec_v_float_cmp(cpu, temps, a, b, prec) {
                    return r;
                }
            }
            IrOp::VCvtFromInt {
                dst,
                src,
                int_size,
                prec,
                signed,
            } => {
                if let Some(r) = exec_v_cvt_from_int(cpu, temps, dst, src, int_size, prec, signed) {
                    return r;
                }
            }
            IrOp::VCvtToInt {
                dst,
                src,
                int_size,
                prec,
                trunc,
                signed,
            } => {
                if let Some(r) = exec_v_cvt_to_int(temps, dst, src, int_size, prec, trunc, signed) {
                    return r;
                }
            }
            IrOp::VCvtFloat { dst, src, from, to } => {
                if let Some(r) = exec_v_cvt_float(cpu, temps, dst, src, from, to) {
                    return r;
                }
            }
            IrOp::VFloatUnary {
                dst,
                a,
                src,
                op,
                prec,
                scalar,
            } => {
                if let Some(r) = exec_v_float_unary(cpu, dst, a, src, op, prec, scalar) {
                    return r;
                }
            }
            IrOp::Jump { target } => {
                if let Some(r) = exec_jump(cpu, temps, target) {
                    return r;
                }
            }
            IrOp::Branch {
                cond,
                taken,
                fallthrough,
            } => {
                if let Some(r) = exec_branch(cpu, cond, taken, fallthrough) {
                    return r;
                }
            }
            IrOp::Call {
                target,
                return_addr,
                slot,
                wrap_sp,
            } => {
                if let Some(r) = exec_call(
                    cpu,
                    mem,
                    temps,
                    cur_addr,
                    target,
                    return_addr,
                    slot,
                    wrap_sp,
                ) {
                    return r;
                }
            }
            IrOp::Ret {
                slot,
                pop_extra,
                wrap_sp,
            } => {
                if let Some(r) = exec_ret(cpu, mem, cur_addr, slot, pop_extra, wrap_sp) {
                    return r;
                }
            }
            IrOp::Syscall => {
                if let Some(r) = exec_syscall(cpu, block_end(ir)) {
                    return r;
                }
            }
            IrOp::PortIo {
                port,
                value,
                size,
                dir_out,
            } => {
                if let Some(r) = exec_port_io(cpu, temps, block_end(ir), port, value, size, dir_out)
                {
                    return r;
                }
            }
            IrOp::Hlt => {
                if let Some(r) = exec_hlt(cpu, block_end(ir)) {
                    return r;
                }
            }
            IrOp::Trap { vector, advance } => {
                if let Some(r) = exec_trap(cpu, cur_addr, vector, advance) {
                    return r;
                }
            }
        }
    }

    // Straight-line block with no control-flow terminator (code ran out): flow on
    // from just past the decoded bytes.
    cpu.rip = block_end(ir);
    StepResult::Continue
}

mod control;
mod integer;
mod vector;

pub(crate) use control::*;
pub(crate) use integer::*;
pub(crate) use vector::*;

fn block_end(ir: &IrBlock) -> u64 {
    ir.guest_start + ir.guest_len as u64
}

/// Packed integer op on `lane`-byte elements (matches the JIT's vector codegen).
/// The four-way SSE bitwise op (`pxor`/`pand`/`por`/`pandn`), shared by the
/// register (`VLogic`) and memory (`VLogicM`) forms so `Andn`'s non-commutative
/// `!a & b` can't drift between two hand-copied matches.
fn vlogic(a: u128, b: u128, op: VLogicOp) -> u128 {
    match op {
        VLogicOp::Xor => a ^ b,
        VLogicOp::And => a & b,
        VLogicOp::Or => a | b,
        VLogicOp::Andn => !a & b,
    }
}

/// `pmovzx`/`pmovsx`: read `16/to` low elements of `from` bytes from `src`, zero- or
/// sign-extend each to `to` bytes, pack into the 128-bit result.
fn pmov_extend(src: u128, from: u8, to: u8, signed: bool) -> u128 {
    let (from, to) = (from as usize, to as usize);
    let count = 16 / to;
    let sb = src.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..count {
        let mut val = 0u64;
        for j in 0..from {
            val |= (sb[i * from + j] as u64) << (8 * j);
        }
        // Sign-extend within a u64 when signed; the low `to` bytes are then written.
        let ext = if signed {
            let bits = from as u32 * 8;
            let sign = 1u64 << (bits - 1);
            if val & sign != 0 {
                val | (u64::MAX << bits)
            } else {
                val
            }
        } else {
            val
        };
        let eb = ext.to_le_bytes();
        out[i * to..i * to + to].copy_from_slice(&eb[..to]);
    }
    u128::from_le_bytes(out)
}

/// SSE4.2 `pcmpistri`/`pcmpestri` (task-168.5.4): the string-compare aggregation that
/// returns an index in ECX plus flags. `len1`/`len2` are the valid element counts (for
/// `pcmpistri` the position of the first null element; for `pcmpestri` the saturated
/// |EAX|/|EDX|). Returns `(ecx, cf, zf, sf, of)`; AF and PF are cleared by the caller.
/// Follows the Intel SDM per-(i,j) validity-override table.
pub fn pcmpstr(
    a: u128,
    b: u128,
    len1: usize,
    len2: usize,
    imm: u8,
) -> (u32, bool, bool, bool, bool) {
    let words = imm & 1 != 0;
    let signed = imm & 2 != 0;
    let agg = (imm >> 2) & 3;
    let polarity = (imm >> 4) & 3;
    let msb = imm & 0x40 != 0;
    let n = if words { 8 } else { 16 };
    let ew = if words { 2 } else { 1 }; // element width in bytes
    let mask = if words { 0xFFFFu128 } else { 0xFF };

    let get = |v: u128, i: usize| -> i32 {
        let raw = ((v >> (i * ew * 8)) & mask) as u32;
        if signed {
            if words {
                raw as u16 as i16 as i32
            } else {
                raw as u8 as i8 as i32
            }
        } else {
            raw as i32
        }
    };
    let a_inv = |j: usize| j >= len1;
    let b_inv = |i: usize| i >= len2;

    // Per-(src2 i, src1 j) boolean after the validity override.
    let overridden = |i: usize, j: usize| -> bool {
        let base = match agg {
            1 => {
                // ranges: even j is the range lower bound (src1[j] <= src2[i]), odd is upper.
                if j & 1 == 0 {
                    get(a, j) <= get(b, i)
                } else {
                    get(a, j) >= get(b, i)
                }
            }
            _ => get(a, j) == get(b, i),
        };
        let (ai, bi) = (a_inv(j), b_inv(i));
        match agg {
            0 | 1 => {
                if ai || bi {
                    false
                } else {
                    base
                }
            }
            2 => {
                if ai && bi {
                    true
                } else if ai != bi {
                    false
                } else {
                    base
                }
            }
            _ => {
                // equal ordered
                if ai {
                    true
                } else if bi {
                    false
                } else {
                    base
                }
            }
        }
    };

    let mut intres1: u32 = 0;
    for i in 0..n {
        let bit = match agg {
            0 => (0..n).any(|j| overridden(i, j)),
            1 => (0..n)
                .step_by(2)
                .any(|j| overridden(i, j) && overridden(i, j + 1)),
            2 => overridden(i, i),
            _ => (0..n).all(|j| {
                if i + j < n {
                    overridden(i + j, j)
                } else {
                    a_inv(j) // past the haystack end: OK only if the needle is exhausted
                }
            }),
        };
        if bit {
            intres1 |= 1 << i;
        }
    }

    // Polarity → IntRes2.
    let nmask: u32 = if words { 0xFF } else { 0xFFFF };
    let intres2 = match polarity {
        1 => (!intres1) & nmask,                    // negate all
        3 => intres1 ^ ((1u32 << len2.min(n)) - 1), // negate only valid src2 positions
        _ => intres1 & nmask,                       // positive
    } & nmask;

    let ecx = if intres2 == 0 {
        n as u32
    } else if msb {
        31 - intres2.leading_zeros()
    } else {
        intres2.trailing_zeros()
    };
    let cf = intres2 != 0;
    let zf = len2 < n;
    let sf = len1 < n;
    let of = intres2 & 1 != 0;
    (ecx, cf, zf, sf, of)
}

/// Valid element count for an implicit-length string (`pcmpistri`): the index of the
/// first null element, or `n` if none.
fn pcmpistr_len(v: u128, words: bool) -> usize {
    let (n, ew, mask) = if words {
        (8usize, 2usize, 0xFFFFu128)
    } else {
        (16, 1, 0xFF)
    };
    (0..n)
        .position(|i| (v >> (i * ew * 8)) & mask == 0)
        .unwrap_or(n)
}

/// SSE4.2 `pcmpistri`/`pcmpestri` (task-168.5.4): run the aggregation over `xmm[a]` and
/// `xmm[b]`, returning `(ecx, cf, zf, sf, of)`. For the explicit form the lengths come
/// from EAX/EDX; otherwise from the first null element. Read-only — the interpreter arm
/// and the JIT helper write ECX/flags through their own state machinery.
pub fn pcmpstr_run(
    cpu: &CpuState,
    a: u8,
    b: u8,
    imm: u8,
    explicit: bool,
) -> (u32, bool, bool, bool, bool) {
    pcmpstr_run_bv(cpu, a, cpu.xmm[b as usize], imm, explicit)
}

/// As [`pcmpstr_run`] but source 2 is supplied as a value (`bv`) rather than read from
/// `cpu.xmm[b]` — the memory-source `pcmpistri/pcmpestri` form (task-195). Source 1 is
/// still `cpu.xmm[a]`; the explicit-length EAX/EDX read is unchanged.
pub fn pcmpstr_run_bv(
    cpu: &CpuState,
    a: u8,
    bv: u128,
    imm: u8,
    explicit: bool,
) -> (u32, bool, bool, bool, bool) {
    let av = cpu.xmm[a as usize];
    let words = imm & 1 != 0;
    let n = if words { 8 } else { 16 };
    let (len1, len2) = if explicit {
        let eax = cpu.gpr[0] as u32 as i32;
        let edx = cpu.gpr[2] as u32 as i32;
        (
            (eax.unsigned_abs() as usize).min(n),
            (edx.unsigned_abs() as usize).min(n),
        )
    } else {
        (pcmpistr_len(av, words), pcmpistr_len(bv, words))
    };
    pcmpstr(av, bv, len1, len2, imm)
}

/// EVEX `valign{d,q}` (task-168.5.6): shift the concatenation `a:b` (a high, b low) right
/// by `shift` elements of `elem` bytes, and return the low `bytes` as 128-bit lanes.
fn valign_lanes(a: [u128; 4], b: [u128; 4], shift: u8, elem: u8, bytes: u16) -> [u128; 4] {
    let bytes = bytes as usize;
    // 2*bytes-byte buffer: low half = b, high half = a.
    let mut buf = [0u8; 128];
    for (i, chunk) in b.iter().enumerate().take(bytes / 16) {
        buf[i * 16..i * 16 + 16].copy_from_slice(&chunk.to_le_bytes());
    }
    for (i, chunk) in a.iter().enumerate().take(bytes / 16) {
        buf[bytes + i * 16..bytes + i * 16 + 16].copy_from_slice(&chunk.to_le_bytes());
    }
    let total_elems = (2 * bytes) / elem as usize;
    let shift_bytes = (shift as usize % total_elems) * elem as usize;
    let mut out = [0u128; 4];
    for (i, slot) in out.iter_mut().enumerate().take(bytes / 16) {
        let mut b16 = [0u8; 16];
        b16.copy_from_slice(&buf[shift_bytes + i * 16..shift_bytes + i * 16 + 16]);
        *slot = u128::from_le_bytes(b16);
    }
    out
}

/// `valign` for the JIT helper (task-168.5.6): shift-and-write into `dst`.
#[allow(clippy::too_many_arguments)]
pub fn exec_valign(cpu: &mut CpuState, dst: u8, a: u8, b: u8, shift: u8, elem: u8, bytes: u16) {
    let r = valign_lanes(
        cpu.vec_lanes(a as usize),
        cpu.vec_lanes(b as usize),
        shift,
        elem,
        bytes,
    );
    cpu.set_vec(dst as usize, r, bytes);
}

/// `vpermt2{b,w,d,q}` (task-195): two-table cross-lane permute — the ONE implementation
/// shared by the interpreter and the JIT helper. `idx` holds per-lane indices into the
/// `2*n` lanes of {table0 = `dst`'s old value, table1 = `tbl`}; the result overwrites
/// `dst` under the mask. `masked` selects the write-masked (merge/zero) vs full write.
#[allow(clippy::too_many_arguments)]
pub fn exec_vpermt2(
    cpu: &mut CpuState,
    dst: u8,
    idx: u8,
    tbl: u8,
    elem: u8,
    k: u8,
    masked: bool,
    zeroing: bool,
    bytes: u16,
    imode: bool,
) {
    // vpermt2: index = idx operand, table0 = old dst. vpermi2: index = old dst, table0 =
    // idx operand. Table1 = tbl register in both; the result overwrites dst.
    let (index, table0) = if imode {
        (cpu.vec_lanes(dst as usize), cpu.vec_lanes(idx as usize))
    } else {
        (cpu.vec_lanes(idx as usize), cpu.vec_lanes(dst as usize))
    };
    let table1 = cpu.vec_lanes(tbl as usize);
    let res = permute2(&index, &table0, &table1, elem, bytes);
    if masked {
        cpu.write_masked(dst as usize, res, k, elem, zeroing, bytes);
    } else {
        cpu.set_vec(dst as usize, res, bytes);
    }
}

/// Single-source cross-lane permute `vperm{d,q}` (vector-index, task-195): the whole
/// register is one table; `dst[i] = src[idx[i] & (n-1)]`. Masked/zeroing per the write-
/// mask. Shared by interp and the JIT helper → jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_vperm1(
    cpu: &mut CpuState,
    dst: u8,
    idx: u8,
    src: u8,
    elem: u8,
    bytes: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let n = bytes as usize / elem as usize;
    let index = cpu.vec_lanes(idx as usize);
    let table = cpu.vec_lanes(src as usize);
    let sel = n - 1; // index masked to log2(n) bits
    let mut res = [0u128; 4];
    for i in 0..n {
        let id = get_velem(&index, i, elem) as usize & sel;
        set_velem(&mut res, i, elem, get_velem(&table, id, elem));
    }
    if masked {
        cpu.write_masked(dst as usize, res, k, elem, zeroing, bytes);
    } else {
        cpu.set_vec(dst as usize, res, bytes);
    }
}

/// Memory-source single-table permute `vperm{d,q} v, idx, [mem]` (task-215): the table is
/// loaded from `[base]` rather than a register. Generic over [`StrMem`] so interp and the
/// JIT helper share it → jit == interp. A load fault stops before any register write.
#[allow(clippy::too_many_arguments)]
pub fn vperm1_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    dst: u8,
    idx: u8,
    base: u64,
    elem: u8,
    k: u8,
    masked: bool,
    zeroing: bool,
    bytes: u16,
    cur_addr: u64,
) -> Option<StrFault> {
    let mut table = [0u128; 4];
    for (i, slot) in table.iter_mut().enumerate().take(bytes as usize / 16) {
        let ea = base.wrapping_add(i as u64 * 16);
        let lo = match mem.sload(ea, 8) {
            Ok(v) => v,
            Err(t) => {
                cpu.rip = cur_addr;
                return Some(StrFault {
                    addr: ea,
                    write: false,
                    trap: t,
                    value: 0,
                    elem: 8,
                });
            }
        };
        let hi = match mem.sload(ea + 8, 8) {
            Ok(v) => v,
            Err(t) => {
                cpu.rip = cur_addr;
                return Some(StrFault {
                    addr: ea + 8,
                    write: false,
                    trap: t,
                    value: 0,
                    elem: 8,
                });
            }
        };
        *slot = (lo as u128) | ((hi as u128) << 64);
    }
    let index = cpu.vec_lanes(idx as usize);
    let n = bytes as usize / elem as usize;
    let sel = n - 1;
    let mut res = [0u128; 4];
    for i in 0..n {
        let id = get_velem(&index, i, elem) as usize & sel;
        set_velem(&mut res, i, elem, get_velem(&table, id, elem));
    }
    if masked {
        cpu.write_masked(dst as usize, res, k, elem, zeroing, bytes);
    } else {
        cpu.set_vec(dst as usize, res, bytes);
    }
    None
}

/// Two-table cross-lane permute core (shared by `vpermt2`/`vpermi2`, reg + memory src):
/// for each of the `bytes/elem` lanes, `index[i]` (masked to `log2(2n)` bits) selects a
/// lane from the concatenation `table0:table1`.
fn permute2(
    index: &[u128; 4],
    table0: &[u128; 4],
    table1: &[u128; 4],
    elem: u8,
    bytes: u16,
) -> [u128; 4] {
    let n = bytes as usize / elem as usize;
    let sel = 2 * n - 1;
    let mut res = [0u128; 4];
    for i in 0..n {
        let id = get_velem(index, i, elem) as usize & sel;
        let v = if id < n {
            get_velem(table0, id, elem)
        } else {
            get_velem(table1, id - n, elem)
        };
        set_velem(&mut res, i, elem, v);
    }
    res
}

/// Memory-source `vpermt2`/`vpermi2` (task-195): table 1 is loaded from `[base]` rather
/// than a register. Generic over [`StrMem`] so interp (`Memory`) and the JIT helper
/// (`RawStrMem`) share it → jit == interp. A load fault stops before any write.
#[allow(clippy::too_many_arguments)]
pub fn permute2_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    dst: u8,
    idx: u8,
    base: u64,
    elem: u8,
    k: u8,
    masked: bool,
    zeroing: bool,
    bytes: u16,
    imode: bool,
    cur_addr: u64,
) -> Option<StrFault> {
    // Load table1 from memory first (fault before any register write).
    let mut table1 = [0u128; 4];
    for (i, slot) in table1.iter_mut().enumerate().take(bytes as usize / 16) {
        let ea = base.wrapping_add(i as u64 * 16);
        let lo = match mem.sload(ea, 8) {
            Ok(v) => v,
            Err(t) => {
                cpu.rip = cur_addr;
                return Some(StrFault {
                    addr: ea,
                    write: false,
                    trap: t,
                    value: 0,
                    elem: 8,
                });
            }
        };
        let hi = match mem.sload(ea + 8, 8) {
            Ok(v) => v,
            Err(t) => {
                cpu.rip = cur_addr;
                return Some(StrFault {
                    addr: ea + 8,
                    write: false,
                    trap: t,
                    value: 0,
                    elem: 8,
                });
            }
        };
        *slot = (lo as u128) | ((hi as u128) << 64);
    }
    let (index, table0) = if imode {
        (cpu.vec_lanes(dst as usize), cpu.vec_lanes(idx as usize))
    } else {
        (cpu.vec_lanes(idx as usize), cpu.vec_lanes(dst as usize))
    };
    let res = permute2(&index, &table0, &table1, elem, bytes);
    if masked {
        cpu.write_masked(dst as usize, res, k, elem, zeroing, bytes);
    } else {
        cpu.set_vec(dst as usize, res, bytes);
    }
    None
}

/// Masked EVEX logic (task-168.5.5): compute `op(a, b)` per 128-bit lane, then write it
/// into `dst` under opmask `k` at `elem` granularity (merge or zeroing).
#[allow(clippy::too_many_arguments)]
fn apply_masked_logic(
    cpu: &mut CpuState,
    op: VLogicOp,
    dst: u8,
    a: u8,
    b: u8,
    k: u8,
    elem: u8,
    zeroing: bool,
    bytes: u16,
) {
    let (al, bl) = (cpu.vec_lanes(a as usize), cpu.vec_lanes(b as usize));
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = vlogic(al[i], bl[i], op);
    }
    cpu.write_masked(dst as usize, r, k, elem, zeroing, bytes);
}

/// EVEX `vpshufb` (task-195): per-128-bit-lane byte shuffle `dst = pshufb(a, idx)` over
/// `bytes`, byte-granularity masked. Shared by interp and the JIT helper → jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_vpshufb_wide(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    idx: u8,
    bytes: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let av = cpu.vec_lanes(a as usize);
    let iv = cpu.vec_lanes(idx as usize);
    let mut res = [0u128; 4];
    for l in 0..(bytes as usize / 16) {
        res[l] = pshufb(av[l], iv[l]);
    }
    if masked {
        cpu.write_masked(dst as usize, res, k, 1, zeroing, bytes);
    } else {
        cpu.set_vec(dst as usize, res, bytes);
    }
}

/// EVEX/VEX-256 `vpshufd` (task-195): per-128-bit-lane dword shuffle by `imm8` over
/// `bytes`, dword-granularity masked. Shared by interp and the JIT helper → jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_vshuffle32_wide(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    imm: u8,
    bytes: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let av = cpu.vec_lanes(a as usize);
    let mut res = [0u128; 4];
    for l in 0..(bytes as usize / 16) {
        let v = av[l];
        let mut r = 0u128;
        for i in 0..4 {
            let sel = (imm >> (2 * i)) & 3;
            let lane = (v >> (sel as u32 * 32)) & 0xffff_ffff;
            r |= lane << (i * 32);
        }
        res[l] = r;
    }
    if masked {
        cpu.write_masked(dst as usize, res, k, 4, zeroing, bytes);
    } else {
        cpu.set_vec(dst as usize, res, bytes);
    }
}

/// Masked EVEX packed arithmetic (task-168.5.5): compute `packed_bin` per 128-bit chunk
/// then merge/zero-mask under `k` at `elem` granularity. Shared by the interpreter and
/// the JIT helper (`exec_masked_packed`) so jit == interp.
#[allow(clippy::too_many_arguments)]
fn apply_masked_packed(
    cpu: &mut CpuState,
    op: PackedBinOp,
    dst: u8,
    a: u8,
    b: u8,
    k: u8,
    elem: u8,
    zeroing: bool,
    bytes: u16,
) {
    let (al, bl) = (cpu.vec_lanes(a as usize), cpu.vec_lanes(b as usize));
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = packed_bin(al[i], bl[i], elem, op);
    }
    cpu.write_masked(dst as usize, r, k, elem, zeroing, bytes);
}

/// EVEX masked packed arithmetic entry for the JIT helper (task-168.5.5). `op_code`
/// mirrors the codegen encoding; delegates to the same [`apply_masked_packed`] the
/// interpreter uses, guaranteeing jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_masked_packed(
    cpu: &mut CpuState,
    op_code: u8,
    dst: u8,
    a: u8,
    b: u8,
    k: u8,
    elem: u8,
    zeroing: bool,
    bytes: u16,
) {
    let op = match op_code {
        0 => PackedBinOp::Add,
        1 => PackedBinOp::Sub,
        2 => PackedBinOp::MinU,
        3 => PackedBinOp::MaxU,
        4 => PackedBinOp::MinS,
        5 => PackedBinOp::MaxS,
        6 => PackedBinOp::MulLo32,
        7 => PackedBinOp::CmpEq,
        8 => PackedBinOp::CmpGt,
        9 => PackedBinOp::MulLo64,
        10 => PackedBinOp::MulU32,
        11 => PackedBinOp::MulS32,
        12 => PackedBinOp::MulLo16,
        13 => PackedBinOp::MulHiU16,
        _ => PackedBinOp::MulHiS16,
    };
    apply_masked_packed(cpu, op, dst, a, b, k, elem, zeroing, bytes);
}

/// EVEX packed shift-by-imm over any width with optional write-masking (task-215).
/// Computes the full unmasked shift per 128-bit lane, then commits: unmasked (`k == 0`)
/// clears above `bytes` via [`CpuState::set_vec`]; masked routes through
/// [`CpuState::write_masked`] for the merge/zero rule. Shared by interp + JIT.
#[allow(clippy::too_many_arguments)]
fn apply_masked_shift(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    imm: u8,
    elem: u8,
    right: bool,
    arith: bool,
    k: u8,
    zeroing: bool,
    bytes: u16,
) {
    let al = cpu.vec_lanes(a as usize);
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = packed_shift(al[i], imm, elem, right, arith);
    }
    if k == 0 {
        cpu.set_vec(dst as usize, r, bytes); // unmasked EVEX: full write, zero-upper
    } else {
        cpu.write_masked(dst as usize, r, k, elem, zeroing, bytes);
    }
}

/// EVEX masked packed shift entry for the JIT helper (task-215); delegates to the same
/// [`apply_masked_shift`] the interpreter uses, guaranteeing jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_masked_shift(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    imm: u8,
    elem: u8,
    right: bool,
    arith: bool,
    k: u8,
    zeroing: bool,
    bytes: u16,
) {
    apply_masked_shift(cpu, dst, a, imm, elem, right, arith, k, zeroing, bytes);
}

/// Packed shift by a scalar register count `vp{sll,srl,sra}{w,d,q} v,v,xmm` (task-215): the
/// low 64 bits of `count`'s xmm shift every lane uniformly. A count ≥ the lane width is
/// clamped to the width so the shared `packed_shift` over-shift path yields 0 / sign-smear.
#[allow(clippy::too_many_arguments)]
pub fn exec_shift_reg(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    count: u8,
    elem: u8,
    right: bool,
    arith: bool,
    k: u8,
    zeroing: bool,
    bytes: u16,
) {
    let cnt = cpu.xmm[count as usize] as u64; // low 64 bits = the uniform shift amount
    let bits = elem as u64 * 8;
    let eff = cnt.min(bits) as u8; // ≥ width → over-shift (packed_shift returns 0 / sign)
    apply_masked_shift(cpu, dst, a, eff, elem, right, arith, k, zeroing, bytes);
}

/// AVX2/AVX-512 per-element variable shift `vp{sll,srl,sra}v{w,d,q}` (task-215): shift each
/// `elem`-byte lane of `a` by the count in the matching lane of `count`, then merge/zero
/// under `k` (`k == 0` = unmasked full-width write). Shared by interp and the JIT helper.
#[allow(clippy::too_many_arguments)]
pub fn exec_var_shift(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    count: u8,
    elem: u8,
    right: bool,
    arith: bool,
    k: u8,
    zeroing: bool,
    bytes: u16,
) {
    let al = cpu.vec_lanes(a as usize);
    let cl = cpu.vec_lanes(count as usize);
    let n = bytes as usize / elem as usize;
    let bits = elem as u32 * 8;
    let mut r = [0u128; 4];
    for i in 0..n {
        let av = get_velem(&al, i, elem);
        let cnt = get_velem(&cl, i, elem);
        set_velem(&mut r, i, elem, var_shift_one(av, cnt, bits, right, arith));
    }
    if k == 0 {
        cpu.set_vec(dst as usize, r, bytes); // unmasked EVEX: full write, zero-upper
    } else {
        cpu.write_masked(dst as usize, r, k, elem, zeroing, bytes);
    }
}

/// GFNI `gf2p8{mulb,affineqb,affineinvqb}` wide/masked (task-215): per-128-bit-lane GF(2⁸)
/// op over `bytes` (reusing the shared [`GfniOp::apply`] math), then merge/zero under `k`
/// (byte-granular; `k == 0` = unmasked). Shared by interp and the JIT helper → jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_gf2p8(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    b: u8,
    imm: u8,
    mode: u8,
    k: u8,
    zeroing: bool,
    bytes: u16,
) {
    let op = crate::ir::GfniOp::from_u8(mode);
    let al = cpu.vec_lanes(a as usize);
    let bl = cpu.vec_lanes(b as usize);
    let mut r = [0u128; 4];
    for lane in 0..(bytes as usize / 16) {
        r[lane] = op.apply(al[lane], bl[lane], imm);
    }
    if k == 0 {
        cpu.set_vec(dst as usize, r, bytes);
    } else {
        cpu.write_masked(dst as usize, r, k, 1, zeroing, bytes); // byte-granular mask
    }
}

/// As [`exec_gf2p8`] but the second source (the affine matrix / multiplier) is a memory
/// operand at `addr` (task-215): read each 128-bit lane from guest memory, then apply the
/// GF(2⁸) op. Shared by the interpreter and the JIT's `gf2p8_mem` helper via [`StrMem`], so
/// the `dst == src1` aliasing case (openssl's `vgf2p8affineqb ymm,ymm,[mem]`) is exact
/// without a scratch register. Returns `Some(fault)` on an unmapped load.
#[allow(clippy::too_many_arguments)]
pub fn gf2p8_mem_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    dst: u8,
    a: u8,
    addr: u64,
    imm: u8,
    mode: u8,
    k: u8,
    zeroing: bool,
    bytes: u16,
) -> Option<StrFault> {
    let op = crate::ir::GfniOp::from_u8(mode);
    let al = cpu.vec_lanes(a as usize);
    let mut r = [0u128; 4];
    for lane in 0..(bytes as usize / 16) {
        let cea = addr.wrapping_add(lane as u64 * 16);
        // Report the fault at the actual failing 8-byte sub-address, not the lane base
        // (task-219): the matrix is two `sload(..,8)` halves, and only one may be
        // unmapped. A real CPU's #PF si_addr points at the touched byte, and because this
        // ONE function is shared by both the interpreter (`&Memory`) and the JIT helper
        // (`RawStrMem`), interp and JIT stay byte-identical on the reported address. The
        // low half is touched first, so it takes precedence when both would fault.
        let sub_fault = |off: u64, t: MemTrap| StrFault {
            addr: cea.wrapping_add(off),
            write: false,
            trap: t,
            value: 0,
            elem: 8,
        };
        let lo = match mem.sload(cea, 8) {
            Ok(v) => v,
            Err(t) => return Some(sub_fault(0, t)),
        };
        let hi = match mem.sload(cea.wrapping_add(8), 8) {
            Ok(v) => v,
            Err(t) => return Some(sub_fault(8, t)),
        };
        let bl = ((hi as u128) << 64) | lo as u128;
        r[lane] = op.apply(al[lane], bl, imm);
    }
    if k == 0 {
        cpu.set_vec(dst as usize, r, bytes);
    } else {
        cpu.write_masked(dst as usize, r, k, 1, zeroing, bytes);
    }
    None
}

/// One element of a variable shift: `av` (low `bits` bits significant) shifted by `cnt`.
/// A count ≥ `bits` yields 0 (logical/left) or the smeared sign (arithmetic right).
fn var_shift_one(av: u64, cnt: u64, bits: u32, right: bool, arith: bool) -> u64 {
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let av = av & mask;
    if cnt >= bits as u64 {
        return if right && arith && (av >> (bits - 1)) & 1 != 0 {
            mask
        } else {
            0
        };
    }
    let c = cnt as u32;
    if !right {
        (av << c) & mask
    } else if !arith {
        av >> c
    } else {
        let se = sign_extend_128(av as u128, bits as u8); // i128, sign-extended lane
        ((se >> c) as u64) & mask
    }
}

/// EVEX narrowing move `vpmov{q,d,w}{d,w,b}` (task-195): truncate each of the
/// `src_width/from` source lanes to its low `to` bytes, packing them contiguously into
/// dst lanes `0..n`; bits above the packed result are zeroed (EVEX dest). Masking is at
/// `to` granularity over the `n` result lanes — masked-off lanes keep the old dst (merge)
/// or clear (zeroing); `write_masked` can't be reused because a sub-16-byte result has
/// zero 128-bit chunks. Shared by interp and the JIT helper → jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_vpmov_narrow(
    cpu: &mut CpuState,
    dst: u8,
    src: u8,
    from: u8,
    to: u8,
    src_width: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let n = src_width as usize / from as usize;
    let s = cpu.vec_lanes(src as usize);
    let old = cpu.vec_lanes(dst as usize);
    let kmask = if masked {
        cpu.kmask[k as usize]
    } else {
        u64::MAX
    };
    let mut res = [0u128; 4];
    for i in 0..n {
        // set_velem masks to `to` bytes, so the wide source lane is truncated on write.
        let out = if (kmask >> i) & 1 != 0 {
            get_velem(&s, i, from)
        } else if zeroing {
            0
        } else {
            get_velem(&old, i, to) // merge: keep the old dst element
        };
        set_velem(&mut res, i, to, out);
    }
    // Lanes above the packed result are always zeroed → store the full 512-bit register.
    cpu.set_vec(dst as usize, res, 64);
}

/// EVEX/VEX-256 widening move `vpmov{s,z}x*` to a wide (or masked) dest (task-195):
/// zero/sign-extend each of the `dst_width/to` low `from`-byte source lanes to `to` bytes.
/// Masking is at `to` granularity over the result lanes; bits above the result (to `VL`)
/// are zeroed. Shared by interp and the JIT helper → jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_vpmov_extend_wide(
    cpu: &mut CpuState,
    dst: u8,
    src: u8,
    from: u8,
    to: u8,
    signed: bool,
    dst_width: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let n = dst_width as usize / to as usize;
    let s = cpu.vec_lanes(src as usize);
    let old = cpu.vec_lanes(dst as usize);
    let kmask = if masked {
        cpu.kmask[k as usize]
    } else {
        u64::MAX
    };
    let bits = from as u32 * 8;
    let mut res = [0u128; 4];
    for i in 0..n {
        let raw = get_velem(&s, i, from);
        // Sign-extend within a u64 when signed; set_velem then masks to `to` bytes.
        let ext = if signed && bits < 64 && (raw & (1u64 << (bits - 1))) != 0 {
            raw | (u64::MAX << bits)
        } else {
            raw
        };
        let out = if (kmask >> i) & 1 != 0 {
            ext
        } else if zeroing {
            0
        } else {
            get_velem(&old, i, to) // merge: keep the old dst element
        };
        set_velem(&mut res, i, to, out);
    }
    cpu.set_vec(dst as usize, res, dst_width);
}

/// Packed absolute value `vpabs{b,w,d,q}` (task-195): per `elem`-byte lane `dst = |src|`
/// (signed; `abs(MIN)` wraps to `MIN`, matching x86). Masking at `elem` granularity; bits
/// above the result (to `VL`) are zeroed. Shared by interp and the JIT helper.
#[allow(clippy::too_many_arguments)]
pub fn exec_vpabs(
    cpu: &mut CpuState,
    dst: u8,
    src: u8,
    elem: u8,
    dst_width: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let n = dst_width as usize / elem as usize;
    let s = cpu.vec_lanes(src as usize);
    let old = cpu.vec_lanes(dst as usize);
    let kmask = if masked {
        cpu.kmask[k as usize]
    } else {
        u64::MAX
    };
    let bits = elem as u32 * 8;
    let mut res = [0u128; 4];
    for i in 0..n {
        let raw = get_velem(&s, i, elem);
        // Sign-extend to i64, take absolute value (wrapping so |MIN| == MIN), then mask.
        let sext = if bits < 64 && (raw & (1u64 << (bits - 1))) != 0 {
            raw | (u64::MAX << bits)
        } else {
            raw
        };
        let abs = (sext as i64).wrapping_abs() as u64;
        let out = if (kmask >> i) & 1 != 0 {
            abs
        } else if zeroing {
            0
        } else {
            get_velem(&old, i, elem) // merge: keep the old dst element
        };
        set_velem(&mut res, i, elem, out);
    }
    cpu.set_vec(dst as usize, res, dst_width);
}

/// Masked EVEX unary lane op `vplzcnt{d,q}` / `vprol{d,q}` / `vpconflict{d,q}` (task-209):
/// per `elem`-byte lane `dst = f(src)`, masked at `elem` granularity, bits above `VL`
/// zeroed. `op` selects the lane function; `imm` is the rotate count (`vprol` only).
/// Shared by interp and the JIT helper.
#[allow(clippy::too_many_arguments)]
pub fn exec_vp_unary_lane(
    cpu: &mut CpuState,
    dst: u8,
    src: u8,
    op: crate::ir::VpUnaryOp,
    imm: u8,
    elem: u8,
    dst_width: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    use crate::ir::VpUnaryOp;
    let n = dst_width as usize / elem as usize;
    let s = cpu.vec_lanes(src as usize);
    let old = cpu.vec_lanes(dst as usize);
    let kmask = if masked {
        cpu.kmask[k as usize]
    } else {
        u64::MAX
    };
    let mut res = [0u128; 4];
    for i in 0..n {
        let raw = get_velem(&s, i, elem);
        let val = match op {
            VpUnaryOp::Lzcnt => {
                if elem == 4 {
                    (raw as u32).leading_zeros() as u64
                } else {
                    raw.leading_zeros() as u64
                }
            }
            VpUnaryOp::Rol => {
                if elem == 4 {
                    (raw as u32).rotate_left(imm as u32) as u64
                } else {
                    raw.rotate_left(imm as u32)
                }
            }
            VpUnaryOp::Conflict => {
                // dst[i] = bitmask of lower lanes j<i whose element equals lane i.
                let mut m = 0u64;
                for j in 0..i {
                    if get_velem(&s, j, elem) == raw {
                        m |= 1u64 << j;
                    }
                }
                m
            }
        };
        let out = if (kmask >> i) & 1 != 0 {
            val
        } else if zeroing {
            0
        } else {
            get_velem(&old, i, elem) // merge: keep the old dst element
        };
        set_velem(&mut res, i, elem, out);
    }
    cpu.set_vec(dst as usize, res, dst_width);
}

/// Masked EVEX blend `vpblendm{d,q}` (task-209): per `elem`-byte lane
/// `dst[i] = k[i] ? b[i] : (zeroing ? 0 : a[i])`, bits above `VL` zeroed. The opmask `k`
/// is the blend control. Shared by interp and the JIT helper.
#[allow(clippy::too_many_arguments)]
pub fn exec_vp_blendm(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    b: u8,
    k: u8,
    elem: u8,
    dst_width: u16,
    zeroing: bool,
) {
    let n = dst_width as usize / elem as usize;
    let av = cpu.vec_lanes(a as usize);
    let bv = cpu.vec_lanes(b as usize);
    let kmask = cpu.kmask[k as usize];
    let mut res = [0u128; 4];
    for i in 0..n {
        let out = if (kmask >> i) & 1 != 0 {
            get_velem(&bv, i, elem)
        } else if zeroing {
            0
        } else {
            get_velem(&av, i, elem)
        };
        set_velem(&mut res, i, elem, out);
    }
    cpu.set_vec(dst as usize, res, dst_width);
}

/// Masked EVEX 128-bit-lane shuffle `vshuff32x4` / `vshuff64x2` (task-209): imm8 selects
/// whole 128-bit lanes — low half of dst from `a`, high half from `b` — then masked at
/// `elem` granularity. Shared by interp and the JIT helper.
#[allow(clippy::too_many_arguments)]
pub fn exec_vshuf_lane(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    b: u8,
    imm: u8,
    elem: u8,
    dst_width: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let nlanes = dst_width as usize / 16; // 128-bit lanes (2 for 256, 4 for 512)
    let bits_per = nlanes.trailing_zeros(); // 1 bit/field for 2 lanes, 2 bits for 4 lanes
    let sel_mask = (nlanes as u8) - 1;
    let al = cpu.vec_lanes(a as usize);
    let bl = cpu.vec_lanes(b as usize);
    let mut shuf = [0u128; 4];
    for (i, slot) in shuf.iter_mut().enumerate().take(nlanes) {
        let field = ((imm >> (i as u32 * bits_per)) & sel_mask) as usize;
        *slot = if i < nlanes / 2 {
            al[field] // low half of dst comes from src1
        } else {
            bl[field] // high half from src2
        };
    }
    if masked {
        cpu.write_masked(dst as usize, shuf, k, elem, zeroing, dst_width);
    } else {
        cpu.set_vec(dst as usize, shuf, dst_width);
    }
}

/// Masked EVEX `vpmultishiftqb` (AVX512-VBMI, task-209): for each qword `q`, output byte
/// `i` = `data.qword[q]` rotated right by `(ctrl.qword[q].byte[i] & 63)`, low 8 bits.
/// Masked at byte granularity. Shared by interp and the JIT helper.
#[allow(clippy::too_many_arguments)]
pub fn exec_vp_multishift(
    cpu: &mut CpuState,
    dst: u8,
    ctrl: u8,
    data: u8,
    dst_width: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let nq = dst_width as usize / 8; // number of qwords
    let cl = cpu.vec_lanes(ctrl as usize);
    let dl = cpu.vec_lanes(data as usize);
    let mut res = [0u128; 4];
    for q in 0..nq {
        let cq = get_velem(&cl, q, 8); // control qword
        let dq = get_velem(&dl, q, 8); // data qword
        let mut outq = 0u64;
        for i in 0..8 {
            let sh = ((cq >> (i * 8)) & 0x3f) as u32; // control byte i, low 6 bits
            let byte = dq.rotate_right(sh) as u8; // 8 bits starting at bit `sh`, wrapping
            outq |= (byte as u64) << (i * 8);
        }
        set_velem(&mut res, q, 8, outq);
    }
    if masked {
        cpu.write_masked(dst as usize, res, k, 1, zeroing, dst_width);
    } else {
        cpu.set_vec(dst as usize, res, dst_width);
    }
}

/// Masked-EVEX-logic entry for the JIT helper (task-168.5.5). `op_code`: 0=Xor 1=And
/// 2=Or 3=Andn. Delegates to the same [`apply_masked_logic`] the interpreter uses, so
/// JIT and interpreter share one implementation.
#[allow(clippy::too_many_arguments)]
pub fn exec_masked_logic(
    cpu: &mut CpuState,
    op_code: u8,
    dst: u8,
    a: u8,
    b: u8,
    k: u8,
    elem: u8,
    zeroing: bool,
    bytes: u16,
) {
    let op = match op_code {
        0 => VLogicOp::Xor,
        1 => VLogicOp::And,
        2 => VLogicOp::Or,
        _ => VLogicOp::Andn,
    };
    apply_masked_logic(cpu, op, dst, a, b, k, elem, zeroing, bytes);
}

/// SSE4.1 variable blend: for each `lane`-byte lane, pick it from `s` when the lane's
/// top bit in `mask` is set, else from `d`.
fn blendv(d: u128, s: u128, mask: u128, lane: u8) -> u128 {
    let bits = lane as u32 * 8;
    let lm = lane_mask(lane);
    let mut r = 0u128;
    for i in 0..(16 / lane as u32) {
        let sh = i * bits;
        let pick = if (mask >> (sh + bits - 1)) & 1 == 1 {
            s
        } else {
            d
        };
        r |= ((pick >> sh) & lm) << sh;
    }
    r
}

/// SSE4.1 `round`: round each lane of `s` per the imm8 `mode` (bits[1:0]: 0 nearest-even,
/// 1 floor, 2 ceil, 3 truncate; bit[2] "use MXCSR" → nearest-even). When `scalar`, only
/// lane 0 is rounded and the other lanes of `d` are preserved.
fn vround(d: u128, s: u128, prec: FPrec, mode: u8, scalar: bool) -> u128 {
    let m = if mode & 4 != 0 { 0 } else { mode & 3 };
    let rnd = |f: f64| match m {
        1 => f.floor(),
        2 => f.ceil(),
        3 => f.trunc(),
        _ => round_ties_even(f),
    };
    let mut out = d;
    match prec {
        FPrec::F32 => {
            let count = if scalar { 1 } else { 4 };
            for i in 0..count {
                let raw = (s >> (i * 32)) as u32;
                let r = rnd(f32::from_bits(raw) as f64) as f32;
                let mask = 0xFFFF_FFFFu128 << (i * 32);
                out = (out & !mask) | ((r.to_bits() as u128) << (i * 32));
            }
        }
        FPrec::F64 => {
            let count = if scalar { 1 } else { 2 };
            for i in 0..count {
                let raw = (s >> (i * 64)) as u64;
                let r = rnd(f64::from_bits(raw));
                let mask = (u64::MAX as u128) << (i * 64);
                out = (out & !mask) | ((r.to_bits() as u128) << (i * 64));
            }
        }
    }
    out
}

/// `vpternlog` bitwise ternary logic: each output bit is `imm8[(a<<2)|(b<<1)|c]` of the
/// three input bits. For each of the 8 index combinations whose `imm` bit is set, OR in
/// the bits where `a`/`b`/`c` match that index's polarity.
fn ternlog(a: u128, b: u128, c: u128, imm: u8) -> u128 {
    let mut r = 0u128;
    for j in 0..8u8 {
        if imm & (1 << j) != 0 {
            let pa = if j & 4 != 0 { a } else { !a };
            let pb = if j & 2 != 0 { b } else { !b };
            let pc = if j & 1 != 0 { c } else { !c };
            r |= pa & pb & pc;
        }
    }
    r
}

/// Replicate the low `elem`-byte element of `low` across all 16 bytes (vpbroadcast).
fn broadcast_elem(low: u128, elem: u8) -> u128 {
    let bits = elem as u32 * 8; // elem ∈ {1,2,4,8} → bits ≤ 64
    let e = low & lane_mask(elem);
    let mut r = 0u128;
    for i in 0..(16 / elem as u32) {
        r |= e << (i * bits);
    }
    r
}

/// Low-`width`-bit mask for opmask ops (`width` ∈ {8,16,32,64}).
fn kwidth_mask(width: u8) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Evaluate a `vpcmp` predicate (imm8 low 3 bits) against a lane ordering.
fn vpcmp_pred(pred: u8, ord: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match pred & 7 {
        0 => ord == Equal,   // EQ
        1 => ord == Less,    // LT
        2 => ord != Greater, // LE
        3 => false,          // FALSE
        4 => ord != Equal,   // NE
        5 => ord != Less,    // GE (NLT)
        6 => ord == Greater, // GT (NLE)
        _ => true,           // TRUE
    }
}

/// EVEX `vpcmp{,u}{b,w,d,q}` → opmask: one bit per `elem`-byte lane across the low
/// `width` bytes of the four 128-bit chunks, comparing signed or unsigned.
/// EVEX `vptestm`/`vptestnm` → opmask: per `elem`-byte lane, `(a & b) != 0` (or `== 0`
/// when `neg`), one bit per lane across the low `width` bytes.
fn vptest_mask(a: [u128; 4], b: [u128; 4], elem: u8, width: u16, neg: bool) -> u64 {
    let bits = elem as u32 * 8;
    let lane_mask = lane_mask(elem);
    let lanes_per_128 = 16 / elem as u32;
    let mut mask = 0u64;
    let mut idx = 0u32;
    for chunk in 0..(width as usize / 16) {
        for l in 0..lanes_per_128 {
            let sh = l * bits;
            let anded = (a[chunk] >> sh) & (b[chunk] >> sh) & lane_mask;
            let set = if neg { anded == 0 } else { anded != 0 };
            if set {
                mask |= 1u64 << idx;
            }
            idx += 1;
        }
    }
    mask
}

fn vpcmp_mask(a: [u128; 4], b: [u128; 4], elem: u8, width: u16, pred: u8, signed: bool) -> u64 {
    let bits = elem as u32 * 8;
    let lane_mask = lane_mask(elem);
    let lanes_per_128 = 16 / elem as u32;
    let mut mask = 0u64;
    let mut idx = 0u32;
    for chunk in 0..(width as usize / 16) {
        for l in 0..lanes_per_128 {
            let sh = l * bits;
            let la = (a[chunk] >> sh) & lane_mask;
            let lb = (b[chunk] >> sh) & lane_mask;
            let ord = if signed {
                sign_extend_128(la, bits as u8).cmp(&sign_extend_128(lb, bits as u8))
            } else {
                la.cmp(&lb)
            };
            if vpcmp_pred(pred, ord) {
                mask |= 1u64 << idx;
            }
            idx += 1;
        }
    }
    mask
}

fn packed_bin(a: u128, b: u128, lane: u8, op: PackedBinOp) -> u128 {
    let bits = lane as u32 * 8;
    let lane_mask = lane_mask(lane);
    let mut res = 0u128;
    let mut i = 0;
    while i < 16 / lane {
        let sh = i as u32 * bits;
        let (la, lb) = ((a >> sh) & lane_mask, (b >> sh) & lane_mask);
        // Signed lane values (sign-extended from `bits`) for the signed ops.
        let (sa, sb) = (
            sign_extend_128(la, bits as u8),
            sign_extend_128(lb, bits as u8),
        );
        let lr = match op {
            PackedBinOp::Add => la.wrapping_add(lb) & lane_mask,
            PackedBinOp::Sub => la.wrapping_sub(lb) & lane_mask,
            PackedBinOp::CmpEq => {
                if la == lb {
                    lane_mask
                } else {
                    0
                }
            }
            PackedBinOp::CmpGt => {
                if sa > sb {
                    lane_mask
                } else {
                    0
                }
            }
            PackedBinOp::MulLo16 | PackedBinOp::MulLo32 | PackedBinOp::MulLo64 => {
                la.wrapping_mul(lb) & lane_mask
            }
            // vpmulhuw/vpmulhw: high `bits` of the unsigned/signed lane×lane product.
            PackedBinOp::MulHiU16 => (la.wrapping_mul(lb) >> bits) & lane_mask,
            PackedBinOp::MulHiS16 => ((sa.wrapping_mul(sb) >> bits) as u128) & lane_mask,
            // vpmuludq: unsigned low-dword × low-dword → full 64-bit lane.
            PackedBinOp::MulU32 => (la & 0xffff_ffff).wrapping_mul(lb & 0xffff_ffff),
            // vpmuldq: signed low-dword × low-dword → full 64-bit lane (sign-extend first).
            PackedBinOp::MulS32 => {
                ((la as u32 as i32 as i64).wrapping_mul(lb as u32 as i32 as i64)) as u64 as u128
            }
            PackedBinOp::MinU => la.min(lb),
            PackedBinOp::MaxU => la.max(lb),
            PackedBinOp::MinS => {
                if sa < sb {
                    la
                } else {
                    lb
                }
            }
            PackedBinOp::MaxS => {
                if sa > sb {
                    la
                } else {
                    lb
                }
            }
        };
        res |= lr << sh;
        i += 1;
    }
    res
}

/// Packed logical shift of each `lane`-byte element by `imm`.
fn packed_shift(a: u128, imm: u8, lane: u8, right: bool, arith: bool) -> u128 {
    let bits = lane as u32 * 8;
    let lane_mask = lane_mask(lane);
    let over = imm as u32 >= bits; // count >= element width
                                   // A logical/left over-shift yields 0; an arithmetic right over-shift yields
                                   // each lane's sign bit smeared across the whole element.
    if over && !(right && arith) {
        return 0;
    }
    let mut res = 0u128;
    let mut i = 0;
    while i < 16 / lane {
        let sh = i as u32 * bits;
        let lv = (a >> sh) & lane_mask;
        let lr = if !right {
            (lv << imm as u32) & lane_mask
        } else if !arith {
            lv >> imm as u32
        } else {
            // arithmetic right: sign-extend the lane, shift, re-mask.
            let sv = sign_extend_128(lv, bits as u8);
            let shifted = if over {
                sv >> (bits - 1)
            } else {
                sv >> imm as u32
            };
            (shifted as u128) & lane_mask
        };
        res |= lr << sh;
        i += 1;
    }
    res
}

/// punpckl*: interleave the low 8 bytes of `a` and `b` at `lane`-byte elements.
fn unpack_low(a: u128, b: u128, lane: u8, high: bool) -> u128 {
    let bits = lane as u32 * 8;
    let lane_mask = lane_mask(lane);
    let n = 8 / lane;
    let base = if high { n as u32 } else { 0 }; // start element: high half or low
    let mut res = 0u128;
    let mut i = 0u32;
    while i < n as u32 {
        let ea = (a >> ((base + i) * bits)) & lane_mask;
        let eb = (b >> ((base + i) * bits)) & lane_mask;
        res |= ea << (2 * i * bits);
        res |= eb << ((2 * i + 1) * bits);
        i += 1;
    }
    res
}

/// packuswb: 8 signed-16 lanes of `a` then `b`, each saturated to `[0,255]`.
/// One 128-bit lane of `pack{ss,us}{wb,dw}`: saturate each `from`-byte source element
/// (read as signed) to `from/2` bytes; `a`'s elements fill the low half, `b`'s the high.
fn pack_lane(a: u128, b: u128, from: u8, signed: bool) -> u128 {
    let fb = from as u32 * 8;
    let tb = (from as u32 / 2) * 8; // result element bits
    let count = 16 / from as usize; // elements per source lane
    let (lo, hi): (i128, i128) = if signed {
        (-(1i128 << (tb - 1)), (1i128 << (tb - 1)) - 1)
    } else {
        (0, (1i128 << tb) - 1)
    };
    let tmask: u128 = (1u128 << tb) - 1;
    let elem = |v: u128, i: usize| -> i128 {
        let raw = (v >> (i as u32 * fb)) & ((1u128 << fb) - 1);
        // sign-extend the source element from `fb` bits (x86 packs read source as signed)
        let sign = 1u128 << (fb - 1);
        if raw & sign != 0 {
            (raw | (!0u128 << fb)) as i128
        } else {
            raw as i128
        }
    };
    let mut res = 0u128;
    for i in 0..count {
        let ca = (elem(a, i).clamp(lo, hi) as u128) & tmask;
        let cb = (elem(b, i).clamp(lo, hi) as u128) & tmask;
        res |= ca << (i as u32 * tb);
        res |= cb << ((count + i) as u32 * tb);
    }
    res
}

/// One FMA element: `±(x*y) ± z` with a single rounding (`f64`/`f32` `mul_add`), returned
/// as the raw bit pattern. `neg_prod` negates the product, `neg_add` the addend.
fn fma_elem(xb: u64, yb: u64, zb: u64, is_f64: bool, neg_prod: bool, neg_add: bool) -> u64 {
    if is_f64 {
        let mut x = f64::from_bits(xb);
        let y = f64::from_bits(yb);
        let mut z = f64::from_bits(zb);
        if neg_prod {
            x = -x;
        }
        if neg_add {
            z = -z;
        }
        x.mul_add(y, z).to_bits()
    } else {
        let mut x = f32::from_bits(xb as u32);
        let y = f32::from_bits(yb as u32);
        let mut z = f32::from_bits(zb as u32);
        if neg_prod {
            x = -x;
        }
        if neg_add {
            z = -z;
        }
        x.mul_add(y, z).to_bits() as u64
    }
}

/// FMA3 per-lane compute (task-201): `dst[i] = ±(x[i]*y[i]) ± z[i]`. Scalar keeps the low
/// element only (the rest of `old` dst is preserved); packed does `bytes/elem` lanes.
/// Shared by interp and the JIT helper → jit == interp.
#[allow(clippy::too_many_arguments)]
fn fma_lanes(
    xv: [u128; 4],
    yv: [u128; 4],
    zv: [u128; 4],
    old: [u128; 4],
    prec: FPrec,
    scalar: bool,
    neg_prod: bool,
    neg_add: bool,
    bytes: u16,
) -> [u128; 4] {
    let elem = prec.bytes();
    let is_f64 = matches!(prec, FPrec::F64);
    let mut res = if scalar { old } else { [0u128; 4] };
    let n = if scalar {
        1
    } else {
        bytes as usize / elem as usize
    };
    for i in 0..n {
        let r = fma_elem(
            get_velem(&xv, i, elem),
            get_velem(&yv, i, elem),
            get_velem(&zv, i, elem),
            is_f64,
            neg_prod,
            neg_add,
        );
        set_velem(&mut res, i, elem, r);
    }
    res
}

/// FMA3 entry for the JIT helper (register form, task-201): reads x/y/z from vector
/// registers, computes via [`fma_lanes`], writes dst. Guarantees jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn exec_fma(
    cpu: &mut CpuState,
    dst: u8,
    x: u8,
    y: u8,
    z: u8,
    prec_f64: bool,
    scalar: bool,
    neg_prod: bool,
    neg_add: bool,
    bytes: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let prec = if prec_f64 { FPrec::F64 } else { FPrec::F32 };
    let xv = cpu.vec_lanes(x as usize);
    let yv = cpu.vec_lanes(y as usize);
    let zv = cpu.vec_lanes(z as usize);
    let old = cpu.vec_lanes(dst as usize);
    let res = fma_lanes(xv, yv, zv, old, prec, scalar, neg_prod, neg_add, bytes);
    // Masked EVEX packed FMA (task-201 AC#3); scalar masked is rejected at lift.
    if masked {
        cpu.write_masked(dst as usize, res, k, prec.bytes(), zeroing, bytes);
    } else {
        let w = if scalar { 16 } else { bytes };
        cpu.set_vec(dst as usize, res, w);
    }
}

/// Replicate the low `chunk` bytes of `src_bytes` across `dst_width` bytes → four lanes
/// (task-214 lane broadcast).
fn broadcast_lane_lanes(src_bytes: &[u8; 64], chunk: usize, dst_width: usize) -> [u128; 4] {
    let mut out = [0u8; 64];
    let mut i = 0;
    while i + chunk <= dst_width && i + chunk <= 64 {
        out[i..i + chunk].copy_from_slice(&src_bytes[0..chunk]);
        i += chunk;
    }
    let mut r = [0u128; 4];
    for (j, slot) in r.iter_mut().enumerate() {
        *slot = u128::from_le_bytes(out[j * 16..j * 16 + 16].try_into().unwrap());
    }
    r
}

/// EVEX lane-broadcast register form (task-214): replicate the low `chunk` bytes of vector
/// `src` across the dest, masked/zeroing at `elem` granularity. Shared by interp + JIT.
#[allow(clippy::too_many_arguments)]
pub fn exec_broadcast_lane(
    cpu: &mut CpuState,
    dst: u8,
    src: u8,
    chunk: u8,
    elem: u8,
    dst_width: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
) {
    let s = cpu.vec_lanes(src as usize);
    let mut sb = [0u8; 64];
    for (j, lane) in s.iter().enumerate() {
        sb[j * 16..j * 16 + 16].copy_from_slice(&lane.to_le_bytes());
    }
    let res = broadcast_lane_lanes(&sb, chunk as usize, dst_width as usize);
    if masked {
        cpu.write_masked(dst as usize, res, k, elem, zeroing, dst_width);
    } else {
        cpu.set_vec(dst as usize, res, dst_width);
    }
}

/// EVEX lane-broadcast memory form (task-214): the `chunk`-byte block is loaded from
/// `[base]` via `StrMem` (fault-capable — returns `Some(StrFault)` on unmapped).
#[allow(clippy::too_many_arguments)]
pub fn broadcast_lane_mem_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    dst: u8,
    base: u64,
    chunk: u8,
    elem: u8,
    dst_width: u16,
    k: u8,
    masked: bool,
    zeroing: bool,
    cur_addr: u64,
) -> Option<StrFault> {
    let mut sb = [0u8; 64];
    // Load the chunk 8 bytes at a time (chunk is 8/16/32).
    let mut off = 0usize;
    while off < chunk as usize {
        match mem.sload(base.wrapping_add(off as u64), 8) {
            Ok(v) => sb[off..off + 8].copy_from_slice(&v.to_le_bytes()),
            Err(t) => {
                cpu.rip = cur_addr;
                return Some(StrFault {
                    addr: base.wrapping_add(off as u64),
                    write: false,
                    trap: t,
                    value: 0,
                    elem: 8,
                });
            }
        }
        off += 8;
    }
    let res = broadcast_lane_lanes(&sb, chunk as usize, dst_width as usize);
    if masked {
        cpu.write_masked(dst as usize, res, k, elem, zeroing, dst_width);
    } else {
        cpu.set_vec(dst as usize, res, dst_width);
    }
    None
}

/// AES-NI round entry for the JIT helper (task-205). Register form: read state `a`
/// and round key `b` from `cpu.xmm`, write `f(a, b)` to `dst`. Shared with interp via
/// [`AesOp::apply`] → jit == interp. `op` is the [`AesOp`] wire value.
pub fn exec_aes(cpu: &mut CpuState, dst: u8, a: u8, b: u8, op: u8) {
    let state = cpu.xmm[a as usize];
    let rk = cpu.xmm[b as usize];
    cpu.xmm[dst as usize] = crate::ir::AesOp::from_u8(op).apply(state, rk);
}

/// AES-NI round entry for the JIT memory-form helper (task-205): the round key is the
/// already-loaded 128-bit value `rk` (the load/fault is done natively before the call).
pub fn exec_aes_mem(cpu: &mut CpuState, dst: u8, a: u8, rk: u128, op: u8) {
    let state = cpu.xmm[a as usize];
    cpu.xmm[dst as usize] = crate::ir::AesOp::from_u8(op).apply(state, rk);
}

/// `aesimc`/`vaesimc` register-form JIT entry (task-205): `dst = InvMixColumns(src)`.
pub fn exec_aes_imc(cpu: &mut CpuState, dst: u8, src: u8) {
    cpu.xmm[dst as usize] = crate::aes::aes_imc(cpu.xmm[src as usize]);
}

/// `aesimc`/`vaesimc` memory-form JIT entry (task-205): source is the loaded value `v`.
pub fn exec_aes_imc_mem(cpu: &mut CpuState, dst: u8, v: u128) {
    cpu.xmm[dst as usize] = crate::aes::aes_imc(v);
}

/// `aeskeygenassist` register-form JIT entry (task-205).
pub fn exec_aes_keygen(cpu: &mut CpuState, dst: u8, src: u8, imm: u8) {
    cpu.xmm[dst as usize] = crate::aes::aes_keygen(cpu.xmm[src as usize], imm);
}

/// `aeskeygenassist` memory-form JIT entry (task-205): source is the loaded value `v`.
pub fn exec_aes_keygen_mem(cpu: &mut CpuState, dst: u8, v: u128, imm: u8) {
    cpu.xmm[dst as usize] = crate::aes::aes_keygen(v, imm);
}

/// SHA-NI register-form JIT entry (task-207): read op1 `a` and op2 `b` from `cpu.xmm`
/// (plus xmm0 for `sha256rnds2`), write `f(a, b, xmm0, imm)` to `dst`. Shared with interp
/// via [`ShaOp::apply`] → jit == interp. `op` is the [`ShaOp`] wire value.
pub fn exec_sha(cpu: &mut CpuState, dst: u8, a: u8, b: u8, imm: u8, op: u8) {
    let x = cpu.xmm[a as usize];
    let y = cpu.xmm[b as usize];
    let xmm0 = cpu.xmm[0];
    cpu.xmm[dst as usize] = crate::ir::ShaOp::from_u8(op).apply(x, y, xmm0, imm);
}

/// SHA-NI memory-form JIT entry (task-207): op2 is the already-loaded 128-bit value `v`
/// (the load/fault is done natively before the call).
pub fn exec_sha_mem(cpu: &mut CpuState, dst: u8, a: u8, v: u128, imm: u8, op: u8) {
    let x = cpu.xmm[a as usize];
    let xmm0 = cpu.xmm[0];
    cpu.xmm[dst as usize] = crate::ir::ShaOp::from_u8(op).apply(x, v, xmm0, imm);
}

/// GFNI register-form JIT entry (task-210): read op1 `a` and op2 `b` from `cpu.xmm`,
/// write `f(a, b, imm)` to `dst`. Shared with interp via [`GfniOp::apply`] → jit == interp.
/// `op` is the [`GfniOp`] wire value; `imm` is the affine constant (ignored by `mulb`).
pub fn exec_gfni(cpu: &mut CpuState, dst: u8, a: u8, b: u8, imm: u8, op: u8) {
    let x = cpu.xmm[a as usize];
    let y = cpu.xmm[b as usize];
    cpu.xmm[dst as usize] = crate::ir::GfniOp::from_u8(op).apply(x, y, imm);
}

/// GFNI memory-form JIT entry (task-210): op2 is the already-loaded 128-bit value `v`
/// (the load/fault is done natively before the call).
pub fn exec_gfni_mem(cpu: &mut CpuState, dst: u8, a: u8, v: u128, imm: u8, op: u8) {
    let x = cpu.xmm[a as usize];
    cpu.xmm[dst as usize] = crate::ir::GfniOp::from_u8(op).apply(x, v, imm);
}

/// PCLMULQDQ register-form JIT entry (task-211): `dst = clmul(a, b, imm)`.
pub fn exec_pclmul(cpu: &mut CpuState, dst: u8, a: u8, b: u8, imm: u8) {
    let x = cpu.xmm[a as usize];
    let y = cpu.xmm[b as usize];
    cpu.xmm[dst as usize] = crate::pclmul::pclmul(x, y, imm);
}

/// PCLMULQDQ memory-form JIT entry (task-211): op2 is the already-loaded 128-bit value `v`
/// (the load/fault is done natively before the call).
pub fn exec_pclmul_mem(cpu: &mut CpuState, dst: u8, a: u8, v: u128, imm: u8) {
    let x = cpu.xmm[a as usize];
    cpu.xmm[dst as usize] = crate::pclmul::pclmul(x, v, imm);
}

/// `movq2dq dst_xmm, src_mm` (task-208): MMX → XMM, shared by interp and the JIT helper.
pub fn exec_movq2dq(cpu: &mut CpuState, dst: u8, src_mm: u8) {
    let lo = u64::from_le_bytes(cpu.fpr[src_mm as usize][0..8].try_into().unwrap());
    cpu.xmm[dst as usize] = lo as u128;
}

/// `movdq2q dst_mm, src_xmm` (task-208): XMM → MMX, shared by interp and the JIT helper.
/// Sets the aliased x87 register's exponent field all-ones (MMX-in-use tag).
pub fn exec_movdq2q(cpu: &mut CpuState, dst_mm: u8, src_xmm: u8) {
    let lo = (cpu.xmm[src_xmm as usize] as u64).to_le_bytes();
    let slot = &mut cpu.fpr[dst_mm as usize];
    slot[0..8].copy_from_slice(&lo);
    slot[8] = 0xff;
    slot[9] = 0xff;
}

/// FMA3 memory-form entry for the JIT helper (task-201): one source (`mem_role`) comes
/// from `[base]`, loaded via `RawStrMem`. Fault-capable — writes the fault and returns
/// `Some(StrFault)` on an unmapped load. Shares [`fma_lanes`] with interp.
#[allow(clippy::too_many_arguments)]
pub fn fma_mem_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    dst: u8,
    x: u8,
    y: u8,
    z: u8,
    base: u64,
    mem_role: u8,
    prec_f64: bool,
    scalar: bool,
    neg_prod: bool,
    neg_add: bool,
    bytes: u16,
    cur_addr: u64,
    writemask: Option<u8>,
    zeroing: bool,
) -> Option<StrFault> {
    let prec = if prec_f64 { FPrec::F64 } else { FPrec::F32 };
    let elem = prec.bytes();
    // Load the memory operand: a scalar (low element) or a full `bytes`-wide vector.
    let mut memv = [0u128; 4];
    let count = if scalar { 1 } else { bytes as usize / 16 };
    for (i, slot) in memv.iter_mut().enumerate().take(count.max(1)) {
        if scalar {
            let lo = match mem.sload(base, elem) {
                Ok(v) => v,
                Err(t) => {
                    cpu.rip = cur_addr;
                    return Some(StrFault {
                        addr: base,
                        write: false,
                        trap: t,
                        value: 0,
                        elem,
                    });
                }
            };
            *slot = lo as u128;
            break;
        }
        let ea = base.wrapping_add(i as u64 * 16);
        let lo = match mem.sload(ea, 8) {
            Ok(v) => v,
            Err(t) => {
                cpu.rip = cur_addr;
                return Some(StrFault {
                    addr: ea,
                    write: false,
                    trap: t,
                    value: 0,
                    elem: 8,
                });
            }
        };
        let hi = match mem.sload(ea + 8, 8) {
            Ok(v) => v,
            Err(t) => {
                cpu.rip = cur_addr;
                return Some(StrFault {
                    addr: ea + 8,
                    write: false,
                    trap: t,
                    value: 0,
                    elem: 8,
                });
            }
        };
        *slot = (lo as u128) | ((hi as u128) << 64);
    }
    let xv = if mem_role == 0 {
        memv
    } else {
        cpu.vec_lanes(x as usize)
    };
    let yv = if mem_role == 1 {
        memv
    } else {
        cpu.vec_lanes(y as usize)
    };
    let zv = if mem_role == 2 {
        memv
    } else {
        cpu.vec_lanes(z as usize)
    };
    let old = cpu.vec_lanes(dst as usize);
    let res = fma_lanes(xv, yv, zv, old, prec, scalar, neg_prod, neg_add, bytes);
    // Masked EVEX packed FMA (task-201 AC#3); scalar masked is rejected at lift.
    match writemask {
        Some(k) => cpu.write_masked(dst as usize, res, k, elem, zeroing, bytes),
        None => {
            let w = if scalar { 16 } else { bytes };
            cpu.set_vec(dst as usize, res, w);
        }
    }
    None
}

/// Pack `pack{ss,us}{wb,dw}` over `bytes` (per 128-bit lane), signed/unsigned saturation.
/// Shared by interp and the JIT helper → jit == interp.
pub fn exec_vpack(
    cpu: &mut CpuState,
    dst: u8,
    a: u8,
    b: u8,
    from_elem: u8,
    signed: bool,
    bytes: u16,
) {
    let (av, bv) = (cpu.vec_lanes(a as usize), cpu.vec_lanes(b as usize));
    let mut res = [0u128; 4];
    for l in 0..(bytes as usize / 16) {
        res[l] = pack_lane(av[l], bv[l], from_elem, signed);
    }
    cpu.set_vec(dst as usize, res, bytes);
}

fn packuswb(a: u128, b: u128) -> u128 {
    let clamp = |w: u128| -> u128 {
        let s = w as u16 as i16;
        s.clamp(0, 255) as u128
    };
    let mut res = 0u128;
    for i in 0..8u32 {
        res |= clamp((a >> (i * 16)) & 0xffff) << (i * 8);
        res |= clamp((b >> (i * 16)) & 0xffff) << ((8 + i) * 8);
    }
    res
}

/// Load a 128-bit vector value (16/8/4 bytes; upper bytes zeroed for <16).
fn vload(mem: &Memory, addr: u64, size: u8) -> Result<u128, MemTrap> {
    match size {
        16 => {
            let lo = mem.read(addr, 8)? as u128;
            let hi = mem.read(addr + 8, 8)? as u128;
            Ok(lo | (hi << 64))
        }
        8 => Ok(mem.read(addr, 8)? as u128),
        _ => Ok(mem.read(addr, 4)? as u128),
    }
}

/// Load a `width`-byte vector (`width/16` 128-bit lanes) from `base` into `[u128; 4]`,
/// zero-filling lanes above `width`. On a fault, returns the faulting 128-bit lane address
/// alongside the trap so the caller can report the exact effective address (task-195).
fn vload_lanes(mem: &Memory, base: u64, width: u16) -> Result<[u128; 4], (u64, MemTrap)> {
    let mut lanes = [0u128; 4];
    for (i, slot) in lanes.iter_mut().enumerate().take(width as usize / 16) {
        let ea = base.wrapping_add(i as u64 * 16);
        *slot = vload(mem, ea, 16).map_err(|t| (ea, t))?;
    }
    Ok(lanes)
}

/// Per-lane population count over a 512-bit vector: each `lane`-byte element (`lane` ∈
/// {4,8} for `vpopcnt{d,q}`) is replaced by the count of its set bits (task-195).
fn vpopcnt_lanes(a: [u128; 4], lane: u8) -> [u128; 4] {
    let bits = lane as u32 * 8;
    let per_128 = 16 / lane as usize;
    let mut r = [0u128; 4];
    for (chunk, out) in a.iter().zip(r.iter_mut()) {
        for l in 0..per_128 {
            let sh = l as u32 * bits;
            let elem = if lane == 8 {
                (*chunk >> sh) as u64
            } else {
                ((*chunk >> sh) as u64) & 0xffff_ffff
            };
            *out |= (elem.count_ones() as u128) << sh;
        }
    }
    r
}

fn vstore(mem: &Memory, addr: u64, v: u128, size: u8) -> Result<(), MemTrap> {
    match size {
        16 => {
            mem.write(addr, v as u64, 8)?;
            mem.write(addr + 8, (v >> 64) as u64, 8)
        }
        8 => mem.write(addr, v as u64, 8),
        _ => mem.write(addr, v as u64 & 0xffff_ffff, 4),
    }
}

/// Set RIP to the faulting instruction and turn a `MemTrap` into the matching Exit.
fn trap_out(
    cpu: &mut CpuState,
    cur_addr: u64,
    trap: MemTrap,
    addr: u64,
    size: u8,
    access: AccessKind,
    value: u64,
) -> StepResult {
    cpu.rip = cur_addr;
    let exit = match (trap, access) {
        (MemTrap::Unmapped, _) => Exit::UnmappedMemory { addr, access },
        (MemTrap::Mmio, AccessKind::Read) => Exit::MmioRead { addr, size },
        (MemTrap::Mmio, _) => Exit::MmioWrite { addr, size, value },
    };
    StepResult::Exit(exit)
}

// --- register access ---

fn read_reg(cpu: &CpuState, reg: Reg) -> u64 {
    match reg.gpr_index() {
        Some(i) => cpu.gpr[i],
        None => match reg {
            Reg::Rip => cpu.rip,
            Reg::FsBase => cpu.fs_base,
            Reg::GsBase => cpu.gs_base,
            _ => unreachable!("gpr_index None only for rip/fs/gs"),
        },
    }
}

fn write_reg(cpu: &mut CpuState, reg: Reg, val: u64, size: u8) {
    match reg.gpr_index() {
        Some(i) => cpu.write_gpr(i, val, size),
        None => match reg {
            Reg::Rip => cpu.rip = val,
            Reg::FsBase => cpu.fs_base = val,
            Reg::GsBase => cpu.gs_base = val,
            _ => unreachable!("gpr_index None only for rip/fs/gs"),
        },
    }
}

fn read_val(v: Val, temps: &[u64]) -> u64 {
    match v {
        Val::Temp(t) => temps[t as usize],
        Val::Imm(i) => i,
    }
}

// --- ALU + flags (Variant A, materialized, §3.2) ---

/// Result of an ALU op: the (masked) value plus the six candidate flags.
struct AluResult {
    res: u64,
    cf: bool,
    pf: bool,
    af: bool,
    zf: bool,
    sf: bool,
    of: bool,
}

fn mask(size: u8) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

fn shift_mask(size: u8) -> u64 {
    if size == 8 {
        63
    } else {
        31
    }
}

fn sign_bit(size: u8) -> u64 {
    1u64 << (size * 8 - 1)
}

const RAX: usize = 0;
const RCX: usize = 1;
const RDX: usize = 2;
const RBX: usize = 3;
const RSI: usize = 6;
const RDI: usize = 7;

/// Guest-memory access for `string_run` (§10). Two implementors give the two
/// backends the memory semantics each already uses for a *scalar* store, so a
/// string op is never quietly weaker than the `mov` next to it:
///
/// * The interpreter passes `&Memory` — every element goes through the same region
///   check + SMC `note_write` as `IrOp::Store`, so `rep stos` onto a code page is
///   caught (§10), a `Trap` region yields MMIO, and an unmapped-but-in-bounds
///   address traps instead of silently scribbling the backing buffer.
/// * The JIT passes [`RawStrMem`] — a bounds-only raw view matching its inlined
///   stores, whose SMC/region handling is the deliberately deferred JIT-side step
///   (§10, §9.1). This keeps the two callers behavior-compatible without pulling
///   the (unavailable) `Memory` into the compiled ABI.
pub trait StrMem {
    fn sload(&self, addr: u64, elem: u8) -> Result<u64, MemTrap>;
    fn sstore(&self, addr: u64, val: u64, elem: u8) -> Result<(), MemTrap>;
}

impl StrMem for Memory {
    fn sload(&self, addr: u64, elem: u8) -> Result<u64, MemTrap> {
        self.read(addr, elem)
    }
    fn sstore(&self, addr: u64, val: u64, elem: u8) -> Result<(), MemTrap> {
        self.write(addr, val, elem)
    }
}

/// Bounds-only raw guest view for the JIT string helper (deferred JIT-side SMC).
/// OOB is the only failure it can report — no region info, so never MMIO.
///
/// `base` is the host address of guest `guest_base`; `size` is the exclusive top guest
/// address (`guest_base + span`). A guest address `a` translates to `base + (a -
/// guest_base)` and is valid iff `guest_base <= a` and `a + elem <= size`. The
/// base-relative offset `a - guest_base` (as a wrapping `u64`) exceeds `size -
/// guest_base` when `a < guest_base`, so the single unsigned bound below rejects
/// below-base and above-top in one comparison (mirrors the JIT's `checked_addr`).
pub struct RawStrMem {
    pub base: *mut u8,
    pub size: u64,
    pub guest_base: u64,
}

impl RawStrMem {
    /// Backing offset for `addr` if `[addr, addr+elem)` lies in `[guest_base, size)`.
    #[inline]
    fn off(&self, addr: u64, elem: u8) -> Option<usize> {
        let end = addr.checked_add(elem as u64)?;
        if addr < self.guest_base || end > self.size {
            return None;
        }
        Some((addr - self.guest_base) as usize)
    }
}

impl StrMem for RawStrMem {
    fn sload(&self, addr: u64, elem: u8) -> Result<u64, MemTrap> {
        let off = self.off(addr, elem).ok_or(MemTrap::Unmapped)?;
        let mut buf = [0u8; 8];
        // SAFETY: bounds-checked into `[guest_base, size)`; `base` is guest `guest_base`.
        unsafe {
            core::ptr::copy_nonoverlapping(self.base.add(off), buf.as_mut_ptr(), elem as usize);
        }
        Ok(u64::from_le_bytes(buf))
    }
    fn sstore(&self, addr: u64, val: u64, elem: u8) -> Result<(), MemTrap> {
        let off = self.off(addr, elem).ok_or(MemTrap::Unmapped)?;
        let bytes = val.to_le_bytes();
        // SAFETY: bounds-checked into `[guest_base, size)`; `base` is guest `guest_base`.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(off), elem as usize);
        }
        Ok(())
    }
}

/// A string op stopped on a memory trap. Carries what the caller needs to build the
/// matching `Exit` (`value`/`elem` matter only for an MMIO write).
pub struct StrFault {
    pub addr: u64,
    pub write: bool,
    pub trap: MemTrap,
    pub value: u64,
    pub elem: u8,
}

/// Execute a (possibly repeated) string op over the raw guest buffer — the ONE
/// implementation shared by the interpreter and the JIT's string helper (§10).
/// Updates RSI/RDI/RCX/RAX/flags; restartable, so on a memory trap it commits the
/// progress made, sets RIP to the faulting instruction, and returns
/// `Some((addr, is_write))`. `None` = ran to completion.
///
/// Memory access goes through [`StrMem`] so the interpreter gets full region + SMC
/// semantics while the JIT keeps its raw view (see the trait docs).
pub fn string_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    op: StrOp,
    elem: u8,
    rep: RepKind,
    cur_addr: u64,
) -> Option<StrFault> {
    let step = if cpu.flags.df {
        (elem as i64).wrapping_neg() as u64
    } else {
        elem as u64
    };
    let m = mask(elem);
    loop {
        if !matches!(rep, RepKind::None) && cpu.gpr[RCX] == 0 {
            break;
        }
        match op {
            StrOp::Movs => {
                let v = match mem.sload(cpu.gpr[RSI], elem) {
                    Ok(v) => v,
                    Err(t) => return trap(cpu, cur_addr, cpu.gpr[RSI], false, t, 0, elem),
                };
                if let Err(t) = mem.sstore(cpu.gpr[RDI], v, elem) {
                    return trap(cpu, cur_addr, cpu.gpr[RDI], true, t, v, elem);
                }
                cpu.gpr[RSI] = cpu.gpr[RSI].wrapping_add(step);
                cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
            }
            StrOp::Stos => {
                let v = cpu.gpr[RAX] & m;
                if let Err(t) = mem.sstore(cpu.gpr[RDI], v, elem) {
                    return trap(cpu, cur_addr, cpu.gpr[RDI], true, t, v, elem);
                }
                cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
            }
            StrOp::Lods => {
                let v = match mem.sload(cpu.gpr[RSI], elem) {
                    Ok(v) => v,
                    Err(t) => return trap(cpu, cur_addr, cpu.gpr[RSI], false, t, 0, elem),
                };
                cpu.write_gpr(RAX, v, elem);
                cpu.gpr[RSI] = cpu.gpr[RSI].wrapping_add(step);
            }
            StrOp::Scas => {
                let b = match mem.sload(cpu.gpr[RDI], elem) {
                    Ok(v) => v,
                    Err(t) => return trap(cpu, cur_addr, cpu.gpr[RDI], false, t, 0, elem),
                };
                let r = alu_sub(cpu.gpr[RAX] & m, b, 0, elem);
                apply(&mut cpu.flags, FlagMask::ALL, &r);
                cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
            }
            StrOp::Cmps => {
                let a = match mem.sload(cpu.gpr[RSI], elem) {
                    Ok(v) => v,
                    Err(t) => return trap(cpu, cur_addr, cpu.gpr[RSI], false, t, 0, elem),
                };
                let b = match mem.sload(cpu.gpr[RDI], elem) {
                    Ok(v) => v,
                    Err(t) => return trap(cpu, cur_addr, cpu.gpr[RDI], false, t, 0, elem),
                };
                let r = alu_sub(a, b, 0, elem);
                apply(&mut cpu.flags, FlagMask::ALL, &r);
                cpu.gpr[RSI] = cpu.gpr[RSI].wrapping_add(step);
                cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
            }
        }
        match rep {
            RepKind::None => break,
            RepKind::Rep => cpu.gpr[RCX] -= 1,
            RepKind::Repe => {
                cpu.gpr[RCX] -= 1;
                if !cpu.flags.zf {
                    break;
                }
            }
            RepKind::Repne => {
                cpu.gpr[RCX] -= 1;
                if cpu.flags.zf {
                    break;
                }
            }
        }
    }
    None
}

/// Build the `Exit` for a memory fault reported by a shared mem helper (`string_run`,
/// `masked_load_run`, `masked_store_run`). RIP is already set by the helper.
fn str_fault_exit(f: StrFault) -> Exit {
    let access = if f.write {
        AccessKind::Write
    } else {
        AccessKind::Read
    };
    match (f.trap, access) {
        (MemTrap::Unmapped, _) => Exit::UnmappedMemory {
            addr: f.addr,
            access,
        },
        (MemTrap::Mmio, AccessKind::Read) => Exit::MmioRead {
            addr: f.addr,
            size: f.elem,
        },
        (MemTrap::Mmio, _) => Exit::MmioWrite {
            addr: f.addr,
            size: f.elem,
            value: f.value,
        },
    }
}

/// Read element `i` (`elem` bytes, `elem` ∈ {1,2,4,8}) from a flattened 512-bit vector.
/// An element never straddles a 128-bit lane (`elem` divides 16), so a single lane
/// suffices.
#[inline]
fn get_velem(lanes: &[u128; 4], i: usize, elem: u8) -> u64 {
    let byte = i * elem as usize;
    let lane = byte / 16;
    let sh = (byte % 16) * 8;
    let mask = if elem == 8 {
        u64::MAX
    } else {
        (1u64 << (elem as u32 * 8)) - 1
    };
    ((lanes[lane] >> sh) as u64) & mask
}

/// Write element `i` (`elem` bytes) into a flattened 512-bit vector (see [`get_velem`]).
#[inline]
fn set_velem(lanes: &mut [u128; 4], i: usize, elem: u8, val: u64) {
    let byte = i * elem as usize;
    let lane = byte / 16;
    let sh = (byte % 16) * 8;
    let mask: u128 = if elem == 8 {
        u64::MAX as u128
    } else {
        (1u128 << (elem as u32 * 8)) - 1
    };
    lanes[lane] = (lanes[lane] & !(mask << sh)) | (((val as u128) & mask) << sh);
}

/// EVEX write-masked vector **load** from memory (`vmovdqu{8,16,32,64} v{k}{z}, [mem]`,
/// task-168.5.5). Element-wise so masked-off lanes never touch memory — hardware fault
/// suppression: a masked-off element straddling an unmapped page must NOT fault. Only
/// active `k` lanes are read; inactive lanes are zeroed (`zeroing`) or kept (merge). On an
/// active-lane fault nothing is committed (a masked load is architecturally atomic): RIP is
/// set to the faulting instruction and the fault returned. Shared by the interpreter and
/// the JIT helper via [`StrMem`], so JIT == interp.
#[allow(clippy::too_many_arguments)]
pub fn masked_load_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    dst: u8,
    base: u64,
    k: u64,
    elem: u8,
    zeroing: bool,
    bytes: u16,
    cur_addr: u64,
) -> Option<StrFault> {
    let n = bytes as usize / elem as usize;
    let mut lanes = cpu.vec_lanes(dst as usize);
    for i in 0..n {
        if k & (1 << i) != 0 {
            let ea = base.wrapping_add((i * elem as usize) as u64);
            match mem.sload(ea, elem) {
                Ok(v) => set_velem(&mut lanes, i, elem, v),
                Err(t) => {
                    cpu.rip = cur_addr;
                    return Some(StrFault {
                        addr: ea,
                        write: false,
                        trap: t,
                        value: 0,
                        elem,
                    });
                }
            }
        } else if zeroing {
            set_velem(&mut lanes, i, elem, 0);
        }
    }
    cpu.set_vec(dst as usize, lanes, bytes);
    None
}

/// EVEX write-masked vector **store** to memory (`vmovdqu{8,16,32,64} [mem]{k}, v`,
/// task-168.5.5). Element-wise, in order — active lanes are stored left to right, so a
/// fault mid-way leaves the earlier active lanes committed (matches hardware; there is no
/// zeroing form for stores). Fault suppression: inactive lanes never touch memory. Shared
/// with the JIT helper via [`StrMem`].
#[allow(clippy::too_many_arguments)]
pub fn masked_store_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    src: u8,
    base: u64,
    k: u64,
    elem: u8,
    bytes: u16,
    cur_addr: u64,
) -> Option<StrFault> {
    let n = bytes as usize / elem as usize;
    let lanes = cpu.vec_lanes(src as usize);
    for i in 0..n {
        if k & (1 << i) != 0 {
            let ea = base.wrapping_add((i * elem as usize) as u64);
            let v = get_velem(&lanes, i, elem);
            if let Err(t) = mem.sstore(ea, v, elem) {
                cpu.rip = cur_addr;
                return Some(StrFault {
                    addr: ea,
                    write: true,
                    trap: t,
                    value: v,
                    elem,
                });
            }
        }
    }
    None
}

/// Unmasked narrowing store `vpmov{q,d,w}{d,w,b} [mem], src` (task-195): truncate each of
/// the `src_width/from` source lanes to `to` bytes and store them contiguously at `base`.
/// A fault stops at the first faulting element (partial stores before it stand, matching
/// hardware). Generic over [`StrMem`] so interp (`Memory`) and the JIT helper (`RawStrMem`)
/// share it → jit == interp.
#[allow(clippy::too_many_arguments)]
pub fn narrow_store_run<M: StrMem>(
    cpu: &mut CpuState,
    mem: &M,
    src: u8,
    from: u8,
    to: u8,
    src_width: u16,
    base: u64,
    cur_addr: u64,
) -> Option<StrFault> {
    let n = src_width as usize / from as usize;
    let lanes = cpu.vec_lanes(src as usize);
    for i in 0..n {
        let v = get_velem(&lanes, i, from); // sstore truncates to `to` bytes on write
        let ea = base.wrapping_add((i * to as usize) as u64);
        if let Err(t) = mem.sstore(ea, v, to) {
            cpu.rip = cur_addr;
            return Some(StrFault {
                addr: ea,
                write: true,
                trap: t,
                value: v,
                elem: to,
            });
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn trap(
    cpu: &mut CpuState,
    cur_addr: u64,
    addr: u64,
    write: bool,
    t: MemTrap,
    value: u64,
    elem: u8,
) -> Option<StrFault> {
    cpu.rip = cur_addr;
    Some(StrFault {
        addr,
        write,
        trap: t,
        value,
        elem,
    })
}

/// Divide the `size`-width `hi:lo` dividend by `divisor` (§16). Returns the
/// (quotient, remainder), or `None` for `#DE` — a zero divisor or a quotient that
/// overflows the destination width. Shared by the interpreter and the JIT's div
/// helper so both agree exactly.
/// `cpuid` (§14): report a plain SSE2 x86-64 — no SSSE3/SSE4/AVX/SHA — so guests
/// pick baseline scalar/SSE2 code paths (e.g. a generic software SHA-256) rather
/// than instruction-set extensions the engine doesn't lift. Shared by both
/// backends (the interpreter calls it directly; the JIT via a helper) so `cpuid`
/// CRC-32C (Castagnoli, SSE4.2 `crc32`): fold the low `bytes` bytes of `src` into
/// the running CRC `crc` using the reflected polynomial 0x82F63B78. Shared by both
/// backends so the checksum matches bit-for-bit.
pub fn crc32c(mut crc: u32, src: u64, bytes: u8) -> u32 {
    for i in 0..bytes as u32 {
        crc ^= ((src >> (i * 8)) & 0xff) as u32;
        for _ in 0..8 {
            let m = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82F6_3B78 & m);
        }
    }
    crc
}

/// answers identically everywhere. Reads leaf in EAX, subleaf in ECX; writes
/// EAX/EBX/ECX/EDX (32-bit, zero-extended).
pub fn cpuid_run(cpu: &mut CpuState) {
    // Feature bits are projected from the embedder-selected set (task-169), NOT
    // hardcoded. The default set reproduces exactly what we advertised before —
    // SSE/SSE2/SSE3/SSSE3/POPCNT/MMX/XSAVE/OSXSAVE/AVX/AVX2 — and is guarded by the
    // compat test `cpuid_advertises_only_what_lifts` (advertise ⊆ lift). A guest's
    // CPUID-dispatched path jumps straight into an instruction after seeing its bit,
    // so an embedder advertising past the lifter is a documented caller risk. The
    // rationale for the default set lives in decision-2/decision-11 (SSE4/BMI/AVX-512
    // stay off by default: unlifted), superseded-as-global by decision-12.
    let f = cpu.features;
    let leaf = cpu.gpr[RAX] as u32;
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32) = match leaf {
        // Max basic leaf + "GenuineIntel".
        0x0 => (0x7, 0x756e_6547, 0x6c65_746e, 0x4965_6e69),
        // Family/model (EAX) + feature flags projected from `f`.
        0x1 => (0x0003_06c3, 0, f.leaf1_ecx(), f.leaf1_edx()),
        // Structured extended features (subleaf 0): AVX2 / BMI / AVX-512 / SHA in EBX,
        // GFNI in ECX (task-211).
        0x7 => (0, f.leaf7_ebx(), f.leaf7_ecx(), 0),
        // Max extended leaf.
        0x8000_0000 => (0x8000_0001, 0, 0, 0),
        // Extended: SYSCALL (EDX 11) + Long Mode (EDX 29); LAHF/LZCNT in ECX.
        0x8000_0001 => (0, 0, f.ext_leaf1_ecx(), (1 << 11) | (1 << 29)),
        _ => (0, 0, 0, 0),
    };
    cpu.write_gpr(RAX, eax as u64, 4);
    cpu.write_gpr(RBX, ebx as u64, 4);
    cpu.write_gpr(RCX, ecx as u64, 4);
    cpu.write_gpr(RDX, edx as u64, 4);
}

/// BMI1/BMI2 result + CF for one op (task-168.5.3). Shared by the interpreter and the
/// JIT's `bmi` helper so both agree exactly; ZF/SF are derived from the result at the
/// call site. `a`/`b` are the raw source values (masked here to `size`).
pub fn bmi_result(a: u64, b: u64, size: u8, op: crate::ir::BmiOp) -> (u64, bool) {
    use crate::ir::BmiOp::*;
    let m = mask(size);
    let bits = size as u32 * 8;
    let (av, bv) = (a & m, b & m);
    match op {
        Andn => (!av & bv & m, false),
        Blsi => (av & av.wrapping_neg() & m, av != 0),
        Blsr => (av & av.wrapping_sub(1) & m, av == 0),
        Blsmsk => ((av ^ av.wrapping_sub(1)) & m, av == 0),
        Bextr => {
            let start = (bv & 0xff) as u32;
            let len = ((bv >> 8) & 0xff) as u32;
            let shifted = if start >= 64 { 0 } else { av >> start };
            let r = if len >= 64 {
                shifted
            } else {
                shifted & ((1u64 << len) - 1)
            };
            (r & m, false)
        }
        Bzhi => {
            let idx = (bv & 0xff) as u32;
            let r = if idx >= bits {
                av
            } else {
                av & ((1u64 << idx) - 1)
            };
            (r & m, idx > bits - 1)
        }
        Pdep => {
            // Deposit a's low bits into the set positions of the mask (no flags).
            let mut r = 0u64;
            let mut k = 0u32;
            for i in 0..bits {
                if (bv >> i) & 1 != 0 {
                    r |= ((av >> k) & 1) << i;
                    k += 1;
                }
            }
            (r & m, false)
        }
        Pext => {
            // Extract a's bits at the set positions of the mask, packed low (no flags).
            let mut r = 0u64;
            let mut k = 0u32;
            for i in 0..bits {
                if (bv >> i) & 1 != 0 {
                    r |= ((av >> i) & 1) << k;
                    k += 1;
                }
            }
            (r & m, false)
        }
    }
}

/// Shared `xgetbv`: EDX:EAX = XCR0 for ECX=0, projected from the guest feature set
/// (task-169). Called by both backends so they answer identically.
pub fn xgetbv_run(cpu: &mut CpuState) {
    cpu.write_gpr(RAX, cpu.features.xcr0(), 4);
    cpu.write_gpr(RDX, 0, 4);
}

pub fn divide(hi: u64, lo: u64, divisor: u64, size: u8, signed: bool) -> Option<(u64, u64)> {
    let m = mask(size);
    let n = size * 8;
    let d = divisor & m;
    if d == 0 {
        return None;
    }
    let dividend = ((hi & m) as u128) << n | (lo & m) as u128;
    if signed {
        let dv = sign_extend(d, size) as i64 as i128;
        let sd = sign_extend_128(dividend, 2 * n);
        // `i128::MIN / -1` (64-bit `idiv` of RDX:RAX = i128::MIN by -1) overflows and
        // would panic; the architecture raises #DE there, same as the quotient-range
        // check below — so fold it into the `None` (→ Exit::Exception vector 0) path.
        let (Some(q), Some(r)) = (sd.checked_div(dv), sd.checked_rem(dv)) else {
            return None;
        };
        let lim = 1i128 << (n - 1);
        if q < -lim || q >= lim {
            return None;
        }
        Some((q as u64 & m, r as u64 & m))
    } else {
        let (q, r) = (dividend / d as u128, dividend % d as u128);
        if q > m as u128 {
            return None;
        }
        Some((q as u64, r as u64))
    }
}

/// Sign-extend the low `bits` bits of a `u128` to a signed `i128`.
fn sign_extend_128(v: u128, bits: u8) -> i128 {
    if bits >= 128 {
        return v as i128;
    }
    let shift = 128 - bits;
    ((v << shift) as i128) >> shift
}

/// Sign-extend the low `from` bytes of `v` to a full 64-bit value.
fn sign_extend(v: u64, from: u8) -> u64 {
    if from >= 8 {
        return v;
    }
    let bits = from * 8;
    let shift = 64 - bits;
    (((v << shift) as i64) >> shift) as u64
}

fn parity(v: u64) -> bool {
    (v as u8).count_ones() % 2 == 0
}

/// `pshufb` byte shuffle: each result byte is selected from `data` by the low
/// nibble of the index byte, or zero when the index's top bit is set.
fn pshufb(data: u128, idx: u128) -> u128 {
    let mut r = 0u128;
    for i in 0..16u32 {
        let sel = (idx >> (i * 8)) & 0xff;
        if sel & 0x80 == 0 {
            let byte = (data >> ((sel as u32 & 0xf) * 8)) & 0xff;
            r |= byte << (i * 8);
        }
    }
    r
}

/// `palignr` (SSSE3): concatenate `dst` (high 16 bytes) with `src` (low 16) into a
/// 32-byte value, shift it right by `imm` bytes, and return the low 16. `imm >= 32`
/// shifts everything out (zero). Branches avoid a shift-by-128 (UB on `u128`).
fn palignr(dst: u128, src: u128, imm: u8) -> u128 {
    let shift = imm as u32 * 8; // bit shift over the 256-bit concatenation
    if imm >= 32 {
        0
    } else if shift == 0 {
        src
    } else if shift < 128 {
        (src >> shift) | (dst << (128 - shift))
    } else if shift == 128 {
        dst
    } else {
        dst >> (shift - 128)
    }
}

/// Low-lane mask for a `bytes`-wide element within a 128-bit value.
fn lane_mask(bytes: u8) -> u128 {
    if bytes >= 16 {
        u128::MAX
    } else {
        (1u128 << (bytes as u32 * 8)) - 1
    }
}

/// `pmovmskb` over one 128-bit register: gather the top bit of each of the 16 bytes into
/// the low 16 bits of the result (bit `i` = byte `i`'s sign).
fn movemask_b(v: u128) -> u64 {
    let mut m = 0u64;
    for i in 0..16 {
        if (v >> (i * 8 + 7)) & 1 != 0 {
            m |= 1 << i;
        }
    }
    m
}

/// Scalar/packed float arithmetic. For `scalar`, only lane 0 is computed and the
/// upper bytes of `a` (= `dst`) are preserved; otherwise every `prec`-wide lane.
fn float_bin(a: u128, b: u128, op: FloatBinOp, prec: FPrec, scalar: bool) -> u128 {
    let bytes = prec.bytes() as u32;
    let lanes = if scalar { 1 } else { 16 / bytes as usize };
    let mut r = a;
    for i in 0..lanes {
        let sh = i as u32 * bytes * 8;
        match prec {
            FPrec::F32 => {
                let z = apply_f32(
                    f32::from_bits((a >> sh) as u32),
                    f32::from_bits((b >> sh) as u32),
                    op,
                );
                r = (r & !(0xffff_ffffu128 << sh)) | ((z.to_bits() as u128) << sh);
            }
            FPrec::F64 => {
                let z = apply_f64(
                    f64::from_bits((a >> sh) as u64),
                    f64::from_bits((b >> sh) as u64),
                    op,
                );
                r = (r & !(0xffff_ffff_ffff_ffffu128 << sh)) | ((z.to_bits() as u128) << sh);
            }
        }
    }
    r
}

/// Scalar/packed float unary op. `dst_old` supplies the preserved upper lanes for
/// the scalar form; `src` is the operand.
fn float_unary(dst_old: u128, src: u128, op: FloatUnOp, prec: FPrec, scalar: bool) -> u128 {
    let bytes = prec.bytes() as u32;
    let lanes = if scalar { 1 } else { 16 / bytes as usize };
    let mut r = dst_old;
    for i in 0..lanes {
        let sh = i as u32 * bytes * 8;
        match prec {
            FPrec::F32 => {
                let v = apply_un_f32(f32::from_bits((src >> sh) as u32), op);
                r = (r & !(0xffff_ffffu128 << sh)) | ((v.to_bits() as u128) << sh);
            }
            FPrec::F64 => {
                let v = apply_un_f64(f64::from_bits((src >> sh) as u64), op);
                r = (r & !(0xffff_ffff_ffff_ffffu128 << sh)) | ((v.to_bits() as u128) << sh);
            }
        }
    }
    r
}

fn apply_un_f32(x: f32, op: FloatUnOp) -> f32 {
    match op {
        FloatUnOp::Sqrt => x.sqrt(),
    }
}

fn apply_un_f64(x: f64, op: FloatUnOp) -> f64 {
    match op {
        FloatUnOp::Sqrt => x.sqrt(),
    }
}

fn apply_f32(x: f32, y: f32, op: FloatBinOp) -> f32 {
    match op {
        FloatBinOp::Add => x + y,
        FloatBinOp::Sub => x - y,
        FloatBinOp::Mul => x * y,
        FloatBinOp::Div => x / y,
        // x86 min/max: the second operand wins on NaN or equal (`x < y` / `x > y`
        // is false there, yielding `y`).
        FloatBinOp::Min => {
            if x < y {
                x
            } else {
                y
            }
        }
        FloatBinOp::Max => {
            if x > y {
                x
            } else {
                y
            }
        }
    }
}

fn apply_f64(x: f64, y: f64, op: FloatBinOp) -> f64 {
    match op {
        FloatBinOp::Add => x + y,
        FloatBinOp::Sub => x - y,
        FloatBinOp::Mul => x * y,
        FloatBinOp::Div => x / y,
        FloatBinOp::Min => {
            if x < y {
                x
            } else {
                y
            }
        }
        FloatBinOp::Max => {
            if x > y {
                x
            } else {
                y
            }
        }
    }
}

/// Round to nearest integer, ties to even (the default MXCSR rounding mode) —
/// `f64::round_ties_even` isn't available at our MSRV. Ties (`|frac| == 0.5`) only
/// occur below 2^52, where `floor as i64` can't overflow.
fn round_ties_even(f: f64) -> f64 {
    let floor = f.floor();
    let diff = f - floor;
    let r = if diff < 0.5 {
        floor
    } else if diff > 0.5 {
        floor + 1.0
    } else if (floor as i64) & 1 == 0 {
        floor
    } else {
        floor + 1.0
    };
    // A zero result keeps the input's sign (IEEE round-to-nearest): round(-0.5) = -0.0,
    // matching the hardware `roundps`/`roundpd`. Harmless for the int-cast caller.
    if r == 0.0 {
        (0.0f64).copysign(f)
    } else {
        r
    }
}

/// `cmpps`-family predicate on two floats. `pred` is the imm8 low 3 bits:
/// 0 EQ, 1 LT, 2 LE, 3 UNORD, 4 NEQ, 5 NLT, 6 NLE, 7 ORD (ordered comparisons are
/// false on a NaN; the "N"/UNORD forms are true).
fn float_pred(ord: Option<Ordering>, pred: u8) -> bool {
    match pred & 7 {
        0 => ord == Some(Ordering::Equal),
        1 => ord == Some(Ordering::Less),
        2 => matches!(ord, Some(Ordering::Less | Ordering::Equal)),
        3 => ord.is_none(),
        4 => ord != Some(Ordering::Equal),
        5 => ord != Some(Ordering::Less),
        6 => !matches!(ord, Some(Ordering::Less | Ordering::Equal)),
        _ => ord.is_some(),
    }
}

/// Per-lane `cmp*` producing an all-ones/zero mask; `scalar` keeps `dst_old`'s
/// upper lanes.
fn float_cmp_mask(dst_old: u128, a: u128, b: u128, prec: FPrec, scalar: bool, pred: u8) -> u128 {
    let bytes = prec.bytes() as u32;
    let lanes = if scalar { 1 } else { 16 / bytes as usize };
    let mut r = dst_old;
    for i in 0..lanes {
        let sh = i as u32 * bytes * 8;
        let ord = match prec {
            FPrec::F32 => {
                f32::from_bits((a >> sh) as u32).partial_cmp(&f32::from_bits((b >> sh) as u32))
            }
            FPrec::F64 => {
                f64::from_bits((a >> sh) as u64).partial_cmp(&f64::from_bits((b >> sh) as u64))
            }
        };
        let m = lane_mask(bytes as u8) << sh;
        r = (r & !m) | if float_pred(ord, pred) { m } else { 0 };
    }
    r
}

/// `ucomis*`/`comis*` flag result `(ZF, PF, CF)`. Unordered (a NaN operand) sets
/// all three; otherwise EQ→ZF, LT→CF, GT→none (x86 §COMISD).
fn float_compare(a: u64, b: u64, prec: FPrec) -> (bool, bool, bool) {
    let ord = match prec {
        FPrec::F32 => f32::from_bits(a as u32).partial_cmp(&f32::from_bits(b as u32)),
        FPrec::F64 => f64::from_bits(a).partial_cmp(&f64::from_bits(b)),
    };
    match ord {
        None => (true, true, true),
        Some(Ordering::Equal) => (true, false, false),
        Some(Ordering::Less) => (false, false, true),
        Some(Ordering::Greater) => (false, false, false),
    }
}

fn alu_add(a: u64, b: u64, carry_in: u64, size: u8) -> AluResult {
    let m = mask(size);
    let (a, b) = (a & m, b & m);
    let wide = a as u128 + b as u128 + carry_in as u128;
    let res = (wide as u64) & m;
    let sb = sign_bit(size);
    AluResult {
        res,
        cf: (wide >> (size * 8)) & 1 != 0,
        pf: parity(res),
        af: ((a & 0xf) + (b & 0xf) + carry_in) & 0x10 != 0,
        zf: res == 0,
        sf: res & sb != 0,
        // signed overflow: operands same sign, result differs.
        of: (!(a ^ b) & (a ^ res)) & sb != 0,
    }
}

fn alu_sub(a: u64, b: u64, borrow_in: u64, size: u8) -> AluResult {
    let m = mask(size);
    let (a, b) = (a & m, b & m);
    let wide = (a as u128)
        .wrapping_sub(b as u128)
        .wrapping_sub(borrow_in as u128);
    let res = (wide as u64) & m;
    let sb = sign_bit(size);
    AluResult {
        res,
        cf: (a as u128) < (b as u128 + borrow_in as u128),
        pf: parity(res),
        af: (a & 0xf) < (b & 0xf) + borrow_in,
        zf: res == 0,
        sf: res & sb != 0,
        // signed overflow: operands differ in sign, result sign != a's sign.
        of: ((a ^ b) & (a ^ res)) & sb != 0,
    }
}

fn rotl(v: u64, cnt: u32, size: u8) -> u64 {
    match size {
        1 => (v as u8).rotate_left(cnt) as u64,
        2 => (v as u16).rotate_left(cnt) as u64,
        4 => (v as u32).rotate_left(cnt) as u64,
        _ => v.rotate_left(cnt),
    }
}

fn rotr(v: u64, cnt: u32, size: u8) -> u64 {
    match size {
        1 => (v as u8).rotate_right(cnt) as u64,
        2 => (v as u16).rotate_right(cnt) as u64,
        4 => (v as u32).rotate_right(cnt) as u64,
        _ => v.rotate_right(cnt),
    }
}

/// Rotate `v` (already masked to `size`) LEFT through CF by `cnt` positions (`cnt`
/// already reduced mod `size*8 + 1`). Returns `(result masked, CF-out)`. Bit-serial —
/// `cnt <= 64`, and rcl/rcr is rare.
fn rcl(mut v: u64, cnt: u32, cf_in: bool, size: u8) -> (u64, bool) {
    let bits = size as u32 * 8;
    let m = mask(size);
    let mut cf = cf_in;
    for _ in 0..cnt {
        let msb = (v >> (bits - 1)) & 1 != 0;
        v = ((v << 1) | cf as u64) & m;
        cf = msb;
    }
    (v, cf)
}

/// Rotate `v` (already masked to `size`) RIGHT through CF by `cnt` positions.
fn rcr(mut v: u64, cnt: u32, cf_in: bool, size: u8) -> (u64, bool) {
    let bits = size as u32 * 8;
    let mut cf = cf_in;
    for _ in 0..cnt {
        let lsb = v & 1 != 0;
        v = (v >> 1) | ((cf as u64) << (bits - 1));
        cf = lsb;
    }
    (v, cf)
}

/// Result carrying only CF/OF (rotates leave the other flags untouched).
fn cf_of(res: u64, cf: bool, of: bool) -> AluResult {
    AluResult {
        res,
        cf,
        pf: false,
        af: false,
        zf: false,
        sf: false,
        of,
    }
}

/// Flags for a shift with a nonzero count: SF/ZF/PF from the result, plus the
/// shift-specific CF and OF (AF is undefined and left out of the SHIFT mask).
fn shift_result(res: u64, size: u8, cf: bool, of: bool) -> AluResult {
    AluResult {
        res,
        cf,
        pf: parity(res),
        af: false,
        zf: res == 0,
        sf: res & sign_bit(size) != 0,
        of,
    }
}

/// Logic ops (and/or/xor/test): CF=OF=0, AF undefined (we clear it), SF/ZF/PF real.
fn alu_logic(res: u64, size: u8) -> AluResult {
    let res = res & mask(size);
    AluResult {
        res,
        cf: false,
        pf: parity(res),
        af: false,
        zf: res == 0,
        sf: res & sign_bit(size) != 0,
        of: false,
    }
}

fn apply(flags: &mut Flags, mask: FlagMask, r: &AluResult) {
    if mask.is_none() {
        return;
    }
    let m = mask.0;
    if m & 0b00_0001 != 0 {
        flags.cf = r.cf;
    }
    if m & 0b00_0010 != 0 {
        flags.pf = r.pf;
    }
    if m & 0b00_0100 != 0 {
        flags.af = r.af;
    }
    if m & 0b00_1000 != 0 {
        flags.zf = r.zf;
    }
    if m & 0b01_0000 != 0 {
        flags.sf = r.sf;
    }
    if m & 0b10_0000 != 0 {
        flags.of = r.of;
    }
}

fn eval_cond(cond: Cond, f: &Flags) -> bool {
    match cond {
        Cond::Eq => f.zf,
        Cond::Ne => !f.zf,
        Cond::Below => f.cf,
        Cond::AboveEq => !f.cf,
        Cond::BelowEq => f.cf || f.zf,
        Cond::Above => !f.cf && !f.zf,
        Cond::Less => f.sf != f.of,
        Cond::GreaterEq => f.sf == f.of,
        Cond::LessEq => (f.sf != f.of) || f.zf,
        Cond::Greater => (f.sf == f.of) && !f.zf,
        Cond::Sign => f.sf,
        Cond::NoSign => !f.sf,
        Cond::Overflow => f.of,
        Cond::NoOverflow => !f.of,
        Cond::Parity => f.pf,
        Cond::NoParity => !f.pf,
    }
}

#[cfg(test)]
mod bmi_tests {
    use super::bmi_result;
    use crate::ir::BmiOp::*;

    #[test]
    fn bmi_result_semantics() {
        // andn: ~a & b
        assert_eq!(bmi_result(0xF0, 0x0F, 1, Andn), (0x0F, false));
        assert_eq!(bmi_result(0xFF, 0x0F, 1, Andn), (0x00, false));
        // blsi: isolate lowest set bit; CF = a != 0
        assert_eq!(bmi_result(0x0C, 0, 4, Blsi), (0x04, true));
        assert_eq!(bmi_result(0, 0, 4, Blsi), (0, false));
        // blsr: clear lowest set bit; CF = a == 0
        assert_eq!(bmi_result(0x0C, 0, 4, Blsr), (0x08, false));
        assert_eq!(bmi_result(0, 0, 4, Blsr), (0, true));
        // blsmsk: mask up to lowest set bit; CF = a == 0
        assert_eq!(bmi_result(0x0C, 0, 4, Blsmsk), (0x07, false));
        assert_eq!(bmi_result(0, 0, 4, Blsmsk), (0xFFFF_FFFF, true));
        // bextr: extract `len` bits from `start` (ctrl = start | len<<8)
        assert_eq!(bmi_result(0xABCD, 4 | (8 << 8), 4, Bextr), (0xBC, false));
        // bzhi: zero bits from index up; CF = index > width-1
        assert_eq!(bmi_result(0xFFFF, 8, 4, Bzhi), (0xFF, false));
        assert_eq!(bmi_result(0xFFFF, 40, 4, Bzhi), (0xFFFF, true)); // idx > 31
                                                                     // pdep: deposit low bits of a into mask positions (1,2,4).
        assert_eq!(bmi_result(0b1011, 0b1_0110, 4, Pdep), (0b0_0110, false));
        // pext: pack a's bits at mask positions (1,2,4) low.
        assert_eq!(bmi_result(0b1_0110, 0b1_0110, 4, Pext), (0b111, false));
    }
}
