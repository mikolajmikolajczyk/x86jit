//! IR interpreter (§8.1). Walks an `IrBlock`'s ops over a `temps` vector and a
//! `&mut CpuState`, reading/writing shared guest `&Memory`. Slow but simple — the
//! oracle the JIT is validated against.
//!
//! RIP-on-trap convention (§8, §16), identical to the JIT's: on a memory trap RIP
//! is set to the FAULTING instruction (`cur_addr`, from `InsnStart`) so the user
//! can map/handle and retry; after `syscall`/`hlt` RIP is PAST the instruction.

use crate::exit::{AccessKind, Exit, StepResult};
use std::cmp::Ordering;

use crate::ir::{
    BtOp, Cond, FPrec, FlagMask, FloatBinOp, FloatUnOp, IrBlock, IrOp, PackedBinOp, RepKind, StrOp,
    Val, VLogicOp,
};
use crate::memory::{MemTrap, Memory};
use crate::state::{CpuState, Flags, Reg};

/// `gpr[]` slot for RSP (used by push/pop-style stack ops in Call/Ret).
const RSP: usize = 4;

pub fn interpret_block(ir: &IrBlock, cpu: &mut CpuState, mem: &Memory) -> StepResult {
    let mut temps = vec![0u64; ir.temp_count as usize];
    let mut cur_addr = ir.guest_start;

    for op in &ir.ops {
        match op {
            IrOp::InsnStart { guest_addr } => cur_addr = *guest_addr,

            IrOp::ReadReg { dst, reg } => temps[*dst as usize] = read_reg(cpu, *reg),
            IrOp::WriteReg { reg, src, size } => {
                let v = read_val(*src, &temps);
                write_reg(cpu, *reg, v, *size);
            }

            IrOp::Add { dst, a, b, size, set_flags } => {
                let r = alu_add(read_val(*a, &temps), read_val(*b, &temps), 0, *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Adc { dst, a, b, size, set_flags } => {
                let c = cpu.flags.cf as u64;
                let r = alu_add(read_val(*a, &temps), read_val(*b, &temps), c, *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Sub { dst, a, b, size, set_flags } => {
                let r = alu_sub(read_val(*a, &temps), read_val(*b, &temps), 0, *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Sbb { dst, a, b, size, set_flags } => {
                let c = cpu.flags.cf as u64;
                let r = alu_sub(read_val(*a, &temps), read_val(*b, &temps), c, *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::And { dst, a, b, size, set_flags } => {
                let r = alu_logic(read_val(*a, &temps) & read_val(*b, &temps), *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Or { dst, a, b, size, set_flags } => {
                let r = alu_logic(read_val(*a, &temps) | read_val(*b, &temps), *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Xor { dst, a, b, size, set_flags } => {
                let r = alu_logic(read_val(*a, &temps) ^ read_val(*b, &temps), *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Shl { dst, a, b, size, set_flags } => {
                let vm = read_val(*a, &temps) & mask(*size);
                let cnt = read_val(*b, &temps) & shift_mask(*size);
                let res = vm.wrapping_shl(cnt as u32) & mask(*size);
                temps[*dst as usize] = res;
                if !set_flags.is_none() && cnt != 0 {
                    let n = (*size * 8) as u64;
                    let cf = cnt <= n && (vm >> (n - cnt)) & 1 != 0;
                    let of = (res & sign_bit(*size) != 0) ^ cf; // count==1 rule
                    apply(&mut cpu.flags, *set_flags, &shift_result(res, *size, cf, of));
                }
            }
            IrOp::Shr { dst, a, b, size, set_flags } => {
                let vm = read_val(*a, &temps) & mask(*size);
                let cnt = read_val(*b, &temps) & shift_mask(*size);
                let res = vm.wrapping_shr(cnt as u32);
                temps[*dst as usize] = res;
                if !set_flags.is_none() && cnt != 0 {
                    let cf = (vm >> (cnt - 1)) & 1 != 0;
                    let of = vm & sign_bit(*size) != 0; // count==1 rule: original MSB
                    apply(&mut cpu.flags, *set_flags, &shift_result(res, *size, cf, of));
                }
            }
            IrOp::Sar { dst, a, b, size, set_flags } => {
                let vm = read_val(*a, &temps) & mask(*size);
                let cnt = read_val(*b, &temps) & shift_mask(*size);
                let res = (sign_extend(vm, *size) as i64 >> cnt) as u64 & mask(*size);
                temps[*dst as usize] = res;
                if !set_flags.is_none() && cnt != 0 {
                    let cf = (vm >> (cnt - 1)) & 1 != 0;
                    apply(&mut cpu.flags, *set_flags, &shift_result(res, *size, cf, false));
                }
            }
            IrOp::Sext { dst, a, from } => {
                temps[*dst as usize] = sign_extend(read_val(*a, &temps), *from);
            }
            IrOp::Bswap { dst, a, size } => {
                let v = read_val(*a, &temps);
                temps[*dst as usize] = if *size == 8 {
                    v.swap_bytes()
                } else {
                    (v as u32).swap_bytes() as u64
                };
            }
            IrOp::Rol { dst, a, b, size, set_flags } => {
                let vm = read_val(*a, &temps) & mask(*size);
                let cnt = read_val(*b, &temps) & shift_mask(*size);
                let res = rotl(vm, cnt as u32, *size);
                temps[*dst as usize] = res;
                if !set_flags.is_none() && cnt != 0 {
                    let cf = res & 1 != 0;
                    let of = (res & sign_bit(*size) != 0) ^ cf; // count==1 rule
                    apply(&mut cpu.flags, *set_flags, &cf_of(res, cf, of));
                }
            }
            IrOp::Ror { dst, a, b, size, set_flags } => {
                let vm = read_val(*a, &temps) & mask(*size);
                let cnt = read_val(*b, &temps) & shift_mask(*size);
                let res = rotr(vm, cnt as u32, *size);
                temps[*dst as usize] = res;
                if !set_flags.is_none() && cnt != 0 {
                    let n = *size * 8;
                    let cf = res & sign_bit(*size) != 0;
                    let of = cf ^ (res >> (n - 2) & 1 != 0); // top two bits differ
                    apply(&mut cpu.flags, *set_flags, &cf_of(res, cf, of));
                }
            }
            IrOp::Mul { lo, hi, a, b, size, signed, set_flags } => {
                let m = mask(*size);
                let n = *size * 8;
                let (va, vb) = (read_val(*a, &temps) & m, read_val(*b, &temps) & m);
                let (lo_v, hi_v, overflow) = if *signed {
                    let p = sign_extend(va, *size) as i64 as i128
                        * sign_extend(vb, *size) as i64 as i128;
                    let lo_v = p as u64 & m;
                    let hi_v = (p >> n) as u64 & m;
                    (lo_v, hi_v, p != sign_extend(lo_v, *size) as i64 as i128)
                } else {
                    let p = va as u128 * vb as u128;
                    let lo_v = p as u64 & m;
                    let hi_v = (p >> n) as u64 & m;
                    (lo_v, hi_v, hi_v != 0)
                };
                temps[*lo as usize] = lo_v;
                temps[*hi as usize] = hi_v;
                if !set_flags.is_none() {
                    // Only CF/OF are defined (the CF_OF mask); the rest are ignored.
                    let r = AluResult {
                        res: lo_v,
                        cf: overflow,
                        pf: false,
                        af: false,
                        zf: false,
                        sf: false,
                        of: overflow,
                    };
                    apply(&mut cpu.flags, *set_flags, &r);
                }
            }

            IrOp::Div { quot, rem, hi, lo, divisor, size, signed } => {
                let hv = read_val(*hi, &temps);
                let lv = read_val(*lo, &temps);
                let dv = read_val(*divisor, &temps);
                match divide(hv, lv, dv, *size, *signed) {
                    Some((q, r)) => {
                        temps[*quot as usize] = q;
                        temps[*rem as usize] = r;
                    }
                    // #DE: RIP on the faulting div; nothing committed to registers yet.
                    None => {
                        cpu.rip = cur_addr;
                        return StepResult::Exit(Exit::Exception { addr: cur_addr, vector: 0 });
                    }
                }
            }

            IrOp::GetCond { dst, cond } => {
                temps[*dst as usize] = eval_cond(*cond, &cpu.flags) as u64;
            }

            IrOp::Load { dst, addr, size } => {
                let a = read_val(*addr, &temps);
                match mem.read(a, *size) {
                    Ok(v) => temps[*dst as usize] = v,
                    Err(t) => return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Read, 0),
                }
            }
            IrOp::Store { addr, src, size, .. } => {
                let a = read_val(*addr, &temps);
                let v = read_val(*src, &temps);
                if let Err(t) = mem.write(a, v, *size) {
                    return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, v);
                }
            }
            IrOp::AtomicRmw { old, addr, src, size, op } => {
                let a = read_val(*addr, &temps);
                let s = read_val(*src, &temps);
                match mem.atomic_rmw(a, s, *size, *op) {
                    Ok(prev) => temps[*old as usize] = prev,
                    Err(t) => return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, s),
                }
            }
            IrOp::AtomicCas { old, addr, expected, src, size } => {
                let a = read_val(*addr, &temps);
                let (exp, s) = (read_val(*expected, &temps), read_val(*src, &temps));
                match mem.atomic_cas(a, exp, s, *size) {
                    Ok(prev) => temps[*old as usize] = prev,
                    Err(t) => return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, s),
                }
            }
            IrOp::Bt { result, a, bit, size, op } => {
                let av = read_val(*a, &temps);
                let b = read_val(*bit, &temps) & (*size as u64 * 8 - 1);
                cpu.flags.cf = (av >> b) & 1 != 0;
                let m = 1u64 << b;
                let r = match op {
                    BtOp::Test => av,
                    BtOp::Set => av | m,
                    BtOp::Reset => av & !m,
                    BtOp::Complement => av ^ m,
                };
                temps[*result as usize] = r & mask(*size);
            }
            IrOp::Cpuid => cpuid_run(cpu),
            IrOp::X87 { kind, addr, sti } => {
                let a = read_val(*addr, &temps);
                let base = mem.host_base() as *mut u8;
                // SAFETY: raw guest access bounds-checked inside exec_x87 against
                // mem.size(); identical to the JIT's x87 helper (shared routine).
                if let Some((fault, write)) =
                    unsafe { crate::x87::exec_x87(cpu, base, mem.size(), *kind, a, *sti) }
                {
                    let access = if write { AccessKind::Write } else { AccessKind::Read };
                    // RIP already on the faulting instruction (cur_addr) via InsnStart.
                    cpu.rip = cur_addr;
                    return StepResult::Exit(Exit::UnmappedMemory { addr: fault, access });
                }
            }
            IrOp::BitScan { dst, src, old, size, reverse } => {
                let s = read_val(*src, &temps) & mask(*size);
                if s == 0 {
                    cpu.flags.zf = true;
                    temps[*dst as usize] = read_val(*old, &temps) & mask(*size);
                } else {
                    cpu.flags.zf = false;
                    temps[*dst as usize] =
                        if *reverse { 63 - s.leading_zeros() as u64 } else { s.trailing_zeros() as u64 };
                }
            }

            IrOp::VLoad { dst, addr, size } => {
                let a = read_val(*addr, &temps);
                match vload(mem, a, *size) {
                    Ok(v) => cpu.xmm[*dst as usize] = v,
                    Err(t) => return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Read, 0),
                }
            }
            IrOp::VStore { addr, src, size } => {
                let a = read_val(*addr, &temps);
                let v = cpu.xmm[*src as usize];
                if let Err(t) = vstore(mem, a, v, *size) {
                    return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, v as u64);
                }
            }
            IrOp::VMov { dst, src } => cpu.xmm[*dst as usize] = cpu.xmm[*src as usize],
            IrOp::VFromGpr { dst, src, size } => {
                let v = read_val(*src, &temps) & mask(*size);
                cpu.xmm[*dst as usize] = v as u128;
            }
            IrOp::VToGpr { dst, src, size } => {
                temps[*dst as usize] = (cpu.xmm[*src as usize] as u64) & mask(*size);
            }
            IrOp::VLogic { dst, a, b, op } => {
                let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                cpu.xmm[*dst as usize] = match op {
                    VLogicOp::Xor => va ^ vb,
                    VLogicOp::And => va & vb,
                    VLogicOp::Or => va | vb,
                    VLogicOp::Andn => !va & vb,
                };
            }
            IrOp::VPackedBin { dst, a, b, lane, op } => {
                let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                cpu.xmm[*dst as usize] = packed_bin(va, vb, *lane, *op);
            }
            IrOp::VPackedBinM { dst, addr, lane, op } => {
                let a = read_val(*addr, &temps);
                match vload(mem, a, 16) {
                    Ok(bv) => {
                        cpu.xmm[*dst as usize] = packed_bin(cpu.xmm[*dst as usize], bv, *lane, *op)
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0),
                }
            }
            IrOp::VLogicM { dst, addr, op } => {
                let a = read_val(*addr, &temps);
                match vload(mem, a, 16) {
                    Ok(bv) => {
                        let va = cpu.xmm[*dst as usize];
                        cpu.xmm[*dst as usize] = match op {
                            VLogicOp::Xor => va ^ bv,
                            VLogicOp::And => va & bv,
                            VLogicOp::Or => va | bv,
                            VLogicOp::Andn => !va & bv,
                        };
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0),
                }
            }
            IrOp::VPackedShift { dst, a, imm, lane, right } => {
                cpu.xmm[*dst as usize] = packed_shift(cpu.xmm[*a as usize], *imm, *lane, *right);
            }
            IrOp::VByteShiftR { dst, a, bytes } => {
                let v = cpu.xmm[*a as usize];
                cpu.xmm[*dst as usize] = if *bytes >= 16 { 0 } else { v >> (*bytes as u32 * 8) };
            }
            IrOp::VShuffle32 { dst, a, imm } => {
                let v = cpu.xmm[*a as usize];
                let mut r = 0u128;
                for i in 0..4 {
                    let sel = (imm >> (2 * i)) & 3;
                    let lane = (v >> (sel as u32 * 32)) & 0xffff_ffff;
                    r |= lane << (i * 32);
                }
                cpu.xmm[*dst as usize] = r;
            }
            IrOp::VMoveHalf { dst, src, dst_high, src_high } => {
                let s = cpu.xmm[*src as usize];
                let half = if *src_high { s >> 64 } else { s & 0xffff_ffff_ffff_ffffu128 };
                let d = cpu.xmm[*dst as usize];
                cpu.xmm[*dst as usize] = if *dst_high {
                    (d & 0xffff_ffff_ffff_ffffu128) | (half << 64)
                } else {
                    (d & !0xffff_ffff_ffff_ffffu128) | half
                };
            }
            IrOp::VLoadHalf { dst, addr, high } => {
                let a = read_val(*addr, &temps);
                match vload(mem, a, 8) {
                    Ok(v) => {
                        let d = cpu.xmm[*dst as usize];
                        cpu.xmm[*dst as usize] = if *high {
                            (d & 0xffff_ffff_ffff_ffffu128) | (v << 64)
                        } else {
                            (d & !0xffff_ffff_ffff_ffffu128) | v
                        };
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, a, 8, AccessKind::Read, 0),
                }
            }
            IrOp::VStoreHalf { addr, src, high } => {
                let a = read_val(*addr, &temps);
                let s = cpu.xmm[*src as usize];
                let half = if *high { s >> 64 } else { s & 0xffff_ffff_ffff_ffffu128 };
                if let Err(t) = vstore(mem, a, half, 8) {
                    return trap_out(cpu, cur_addr, t, a, 8, AccessKind::Write, half as u64);
                }
            }
            IrOp::VExtractW { dst, src, index } => {
                let sh = (*index as u32 & 7) * 16;
                temps[*dst as usize] = ((cpu.xmm[*src as usize] >> sh) & 0xffff) as u64;
            }
            IrOp::VMoveMaskB { dst, src } => {
                let v = cpu.xmm[*src as usize];
                let mut m = 0u64;
                for i in 0..16 {
                    if (v >> (i * 8 + 7)) & 1 != 0 {
                        m |= 1 << i;
                    }
                }
                temps[*dst as usize] = m;
            }
            IrOp::VShuffle16 { dst, a, imm, high } => {
                let v = cpu.xmm[*a as usize];
                let base = if *high { 4u32 } else { 0 };
                let keep = if *high {
                    v & 0xffff_ffff_ffff_ffffu128 // preserve low 64
                } else {
                    v & !0xffff_ffff_ffff_ffffu128 // preserve high 64
                };
                let mut shuf = 0u128;
                for i in 0..4 {
                    let sel = (imm >> (2 * i)) & 3;
                    let w = (v >> ((base + sel as u32) * 16)) & 0xffff;
                    shuf |= w << ((base + i as u32) * 16);
                }
                cpu.xmm[*dst as usize] = keep | shuf;
            }
            IrOp::VUnpackLow { dst, a, b, lane } => {
                cpu.xmm[*dst as usize] =
                    unpack_low(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *lane);
            }
            IrOp::VPackUsWB { dst, a, b } => {
                cpu.xmm[*dst as usize] = packuswb(cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
            }
            IrOp::SetDf { value } => cpu.flags.df = *value,
            IrOp::RepString { op, elem, rep } => {
                let base = mem.host_base() as *mut u8;
                // SAFETY: raw guest-buffer access bounds-checked against mem.size();
                // matches the JIT's string helper exactly (shared routine).
                if let Some((addr, write)) =
                    unsafe { string_run(cpu, base, mem.size(), *op, *elem, *rep, cur_addr) }
                {
                    let access = if write { AccessKind::Write } else { AccessKind::Read };
                    return StepResult::Exit(Exit::UnmappedMemory { addr, access });
                }
            }
            IrOp::VInsertW { dst, src, index } => {
                let v = read_val(*src, &temps) as u16 as u128;
                let sh = (*index as u32 & 7) * 16;
                let old = cpu.xmm[*dst as usize];
                cpu.xmm[*dst as usize] = (old & !(0xffffu128 << sh)) | (v << sh);
            }
            IrOp::VFloatMov { dst, src, prec } => {
                let m = lane_mask(prec.bytes());
                let s = cpu.xmm[*src as usize] & m;
                cpu.xmm[*dst as usize] = (cpu.xmm[*dst as usize] & !m) | s;
            }
            IrOp::VFloatBin { dst, a, b, op, prec, scalar } => {
                let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                cpu.xmm[*dst as usize] = float_bin(va, vb, *op, *prec, *scalar);
            }
            IrOp::VFloatBinM { dst, addr, op, prec, scalar } => {
                let a = read_val(*addr, &temps);
                let size = if *scalar { prec.bytes() } else { 16 };
                match vload(mem, a, size) {
                    Ok(bv) => {
                        cpu.xmm[*dst as usize] =
                            float_bin(cpu.xmm[*dst as usize], bv, *op, *prec, *scalar)
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, a, size, AccessKind::Read, 0),
                }
            }
            IrOp::VFloatCmp { a, b, prec } => {
                let (zf, pf, cf) = float_compare(read_val(*a, &temps), read_val(*b, &temps), *prec);
                cpu.flags.zf = zf;
                cpu.flags.pf = pf;
                cpu.flags.cf = cf;
                cpu.flags.of = false;
                cpu.flags.sf = false;
                cpu.flags.af = false;
            }
            IrOp::VCvtFromInt { dst, src, int_size, prec } => {
                let signed = sign_extend(read_val(*src, &temps), *int_size) as i64;
                let bits = match prec {
                    FPrec::F32 => (signed as f32).to_bits() as u128,
                    FPrec::F64 => (signed as f64).to_bits() as u128,
                };
                let m = lane_mask(prec.bytes());
                cpu.xmm[*dst as usize] = (cpu.xmm[*dst as usize] & !m) | (bits & m);
            }
            IrOp::VCvtToInt { dst, src, int_size, prec, trunc } => {
                let raw = read_val(*src, &temps);
                let f = match prec {
                    FPrec::F32 => f32::from_bits(raw as u32) as f64,
                    FPrec::F64 => f64::from_bits(raw),
                };
                let f = if *trunc { f.trunc() } else { round_ties_even(f) };
                // Saturating cast to the destination width (Rust `as` clamps to
                // INT_MIN/MAX); matches the JIT's `fcvt_to_sint_sat`. The x86
                // integer-indefinite result on invalid operands is deferred.
                temps[*dst as usize] = match int_size {
                    8 => f as i64 as u64,
                    _ => f as i32 as u32 as u64,
                };
            }
            IrOp::VCvtFloat { dst, src, from, to } => {
                let raw = read_val(*src, &temps);
                let val = match from {
                    FPrec::F32 => f32::from_bits(raw as u32) as f64,
                    FPrec::F64 => f64::from_bits(raw),
                };
                let bits = match to {
                    FPrec::F32 => (val as f32).to_bits() as u128,
                    FPrec::F64 => val.to_bits() as u128,
                };
                let m = lane_mask(to.bytes());
                cpu.xmm[*dst as usize] = (cpu.xmm[*dst as usize] & !m) | (bits & m);
            }
            IrOp::VFloatUnary { dst, src, op, prec, scalar } => {
                cpu.xmm[*dst as usize] =
                    float_unary(cpu.xmm[*dst as usize], cpu.xmm[*src as usize], *op, *prec, *scalar);
            }

            IrOp::Jump { target } => {
                cpu.rip = read_val(*target, &temps);
                return StepResult::Continue;
            }
            IrOp::Branch { cond, taken, fallthrough } => {
                cpu.rip = if eval_cond(*cond, &cpu.flags) {
                    *taken
                } else {
                    *fallthrough
                };
                return StepResult::Continue;
            }
            IrOp::Call { target, return_addr } => {
                let sp = cpu.gpr[RSP].wrapping_sub(8);
                if let Err(t) = mem.write(sp, *return_addr, 8) {
                    return trap_out(cpu, cur_addr, t, sp, 8, AccessKind::Write, *return_addr);
                }
                cpu.gpr[RSP] = sp;
                cpu.rip = read_val(*target, &temps);
                return StepResult::Continue;
            }
            IrOp::Ret => {
                let sp = cpu.gpr[RSP];
                match mem.read(sp, 8) {
                    Ok(ret) => {
                        cpu.gpr[RSP] = sp.wrapping_add(8);
                        cpu.rip = ret;
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, sp, 8, AccessKind::Read, 0),
                }
                return StepResult::Continue;
            }
            IrOp::Syscall => {
                cpu.rip = block_end(ir);
                return StepResult::Exit(Exit::Syscall);
            }
            IrOp::Hlt => {
                cpu.rip = block_end(ir);
                return StepResult::Exit(Exit::Hlt);
            }
        }
    }

    // Straight-line block with no control-flow terminator (code ran out): flow on
    // from just past the decoded bytes.
    cpu.rip = block_end(ir);
    StepResult::Continue
}

fn block_end(ir: &IrBlock) -> u64 {
    ir.guest_start + ir.guest_len as u64
}

/// Packed integer op on `lane`-byte elements (matches the JIT's vector codegen).
fn packed_bin(a: u128, b: u128, lane: u8, op: PackedBinOp) -> u128 {
    let bits = lane as u32 * 8;
    let lane_mask: u128 = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
    let mut res = 0u128;
    let mut i = 0;
    while i < 16 / lane {
        let sh = i as u32 * bits;
        let (la, lb) = ((a >> sh) & lane_mask, (b >> sh) & lane_mask);
        // Signed lane values (sign-extended from `bits`) for the signed ops.
        let sign = 1u128 << (bits - 1);
        let (sa, sb) = ((la ^ sign).wrapping_sub(sign), (lb ^ sign).wrapping_sub(sign));
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
                if (sa as i128) > (sb as i128) {
                    lane_mask
                } else {
                    0
                }
            }
            PackedBinOp::MinU => la.min(lb),
            PackedBinOp::MaxU => la.max(lb),
            PackedBinOp::MinS => {
                if (sa as i128) < (sb as i128) {
                    la
                } else {
                    lb
                }
            }
            PackedBinOp::MaxS => {
                if (sa as i128) > (sb as i128) {
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
fn packed_shift(a: u128, imm: u8, lane: u8, right: bool) -> u128 {
    let bits = lane as u32 * 8;
    let lane_mask: u128 = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
    if imm as u32 >= bits {
        return 0;
    }
    let mut res = 0u128;
    let mut i = 0;
    while i < 16 / lane {
        let sh = i as u32 * bits;
        let lv = (a >> sh) & lane_mask;
        let lr = if right {
            lv >> imm as u32
        } else {
            (lv << imm as u32) & lane_mask
        };
        res |= lr << sh;
        i += 1;
    }
    res
}

/// punpckl*: interleave the low 8 bytes of `a` and `b` at `lane`-byte elements.
fn unpack_low(a: u128, b: u128, lane: u8) -> u128 {
    let bits = lane as u32 * 8;
    let lane_mask: u128 = (1u128 << bits) - 1;
    let n = 8 / lane;
    let mut res = 0u128;
    let mut i = 0u32;
    while i < n as u32 {
        let ea = (a >> (i * bits)) & lane_mask;
        let eb = (b >> (i * bits)) & lane_mask;
        res |= ea << (2 * i * bits);
        res |= eb << ((2 * i + 1) * bits);
        i += 1;
    }
    res
}

/// packuswb: 8 signed-16 lanes of `a` then `b`, each saturated to `[0,255]`.
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

unsafe fn raw_read(base: *const u8, mem_size: u64, addr: u64, elem: u8) -> Option<u64> {
    if addr.checked_add(elem as u64)? > mem_size {
        return None;
    }
    let mut buf = [0u8; 8];
    core::ptr::copy_nonoverlapping(base.add(addr as usize), buf.as_mut_ptr(), elem as usize);
    Some(u64::from_le_bytes(buf))
}

unsafe fn raw_write(base: *mut u8, mem_size: u64, addr: u64, val: u64, elem: u8) -> bool {
    if addr.checked_add(elem as u64).map_or(true, |e| e > mem_size) {
        return false;
    }
    let bytes = val.to_le_bytes();
    core::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(addr as usize), elem as usize);
    true
}

/// Execute a (possibly repeated) string op over the raw guest buffer — the ONE
/// implementation shared by the interpreter and the JIT's string helper (§10).
/// Updates RSI/RDI/RCX/RAX/flags; restartable, so on a memory trap it commits the
/// progress made, sets RIP to the faulting instruction, and returns
/// `Some((addr, is_write))`. `None` = ran to completion.
///
/// # Safety
/// `base` must point at the guest buffer of `mem_size` bytes for the call.
#[allow(clippy::too_many_arguments)]
pub unsafe fn string_run(
    cpu: &mut CpuState,
    base: *mut u8,
    mem_size: u64,
    op: StrOp,
    elem: u8,
    rep: RepKind,
    cur_addr: u64,
) -> Option<(u64, bool)> {
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
                let v = match raw_read(base, mem_size, cpu.gpr[RSI], elem) {
                    Some(v) => v,
                    None => return trap(cpu, cur_addr, cpu.gpr[RSI], false),
                };
                if !raw_write(base, mem_size, cpu.gpr[RDI], v, elem) {
                    return trap(cpu, cur_addr, cpu.gpr[RDI], true);
                }
                cpu.gpr[RSI] = cpu.gpr[RSI].wrapping_add(step);
                cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
            }
            StrOp::Stos => {
                if !raw_write(base, mem_size, cpu.gpr[RDI], cpu.gpr[RAX] & m, elem) {
                    return trap(cpu, cur_addr, cpu.gpr[RDI], true);
                }
                cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
            }
            StrOp::Lods => {
                let v = match raw_read(base, mem_size, cpu.gpr[RSI], elem) {
                    Some(v) => v,
                    None => return trap(cpu, cur_addr, cpu.gpr[RSI], false),
                };
                cpu.write_gpr(RAX, v, elem);
                cpu.gpr[RSI] = cpu.gpr[RSI].wrapping_add(step);
            }
            StrOp::Scas => {
                let b = match raw_read(base, mem_size, cpu.gpr[RDI], elem) {
                    Some(v) => v,
                    None => return trap(cpu, cur_addr, cpu.gpr[RDI], false),
                };
                let r = alu_sub(cpu.gpr[RAX] & m, b, 0, elem);
                apply(&mut cpu.flags, FlagMask::ALL, &r);
                cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
            }
            StrOp::Cmps => {
                let a = match raw_read(base, mem_size, cpu.gpr[RSI], elem) {
                    Some(v) => v,
                    None => return trap(cpu, cur_addr, cpu.gpr[RSI], false),
                };
                let b = match raw_read(base, mem_size, cpu.gpr[RDI], elem) {
                    Some(v) => v,
                    None => return trap(cpu, cur_addr, cpu.gpr[RDI], false),
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

fn trap(cpu: &mut CpuState, cur_addr: u64, addr: u64, write: bool) -> Option<(u64, bool)> {
    cpu.rip = cur_addr;
    Some((addr, write))
}

/// Divide the `size`-width `hi:lo` dividend by `divisor` (§16). Returns the
/// (quotient, remainder), or `None` for `#DE` — a zero divisor or a quotient that
/// overflows the destination width. Shared by the interpreter and the JIT's div
/// helper so both agree exactly.
/// `cpuid` (§14): report a plain SSE2 x86-64 — no SSSE3/SSE4/AVX/SHA — so guests
/// pick baseline scalar/SSE2 code paths (e.g. a generic software SHA-256) rather
/// than instruction-set extensions the engine doesn't lift. Shared by both
/// backends (the interpreter calls it directly; the JIT via a helper) so `cpuid`
/// answers identically everywhere. Reads leaf in EAX, subleaf in ECX; writes
/// EAX/EBX/ECX/EDX (32-bit, zero-extended).
pub fn cpuid_run(cpu: &mut CpuState) {
    let leaf = cpu.gpr[RAX] as u32;
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32) = match leaf {
        // Max basic leaf + "GenuineIntel".
        0x0 => (0x7, 0x756e_6547, 0x6c65_746e, 0x4965_6e69),
        // Family/model + feature flags. EDX: FPU|TSC|CX8|CMOV|MMX|FXSR|SSE|SSE2.
        // ECX: none (no SSE3/SSSE3/SSE4/AVX). EBX: no APIC/brand.
        0x1 => {
            let edx = (1 << 0)   // FPU
                | (1 << 4)       // TSC
                | (1 << 8)       // CX8 (cmpxchg8b)
                | (1 << 15)      // CMOV
                | (1 << 23)      // MMX
                | (1 << 24)      // FXSR
                | (1 << 25)      // SSE
                | (1 << 26); // SSE2
            (0x0003_06c3, 0, 0, edx)
        }
        // Structured extended features (subleaf 0): no SHA (bit 29), no AVX2/BMI.
        0x7 => (0, 0, 0, 0),
        // Max extended leaf.
        0x8000_0000 => (0x8000_0001, 0, 0, 0),
        // Extended features: SYSCALL (bit 11) + Long Mode (bit 29).
        0x8000_0001 => (0, 0, 0, (1 << 11) | (1 << 29)),
        _ => (0, 0, 0, 0),
    };
    cpu.write_gpr(RAX, eax as u64, 4);
    cpu.write_gpr(RBX, ebx as u64, 4);
    cpu.write_gpr(RCX, ecx as u64, 4);
    cpu.write_gpr(RDX, edx as u64, 4);
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
        let (q, r) = (sd / dv, sd % dv);
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

/// Low-lane mask for a `bytes`-wide element within a 128-bit value.
fn lane_mask(bytes: u8) -> u128 {
    if bytes >= 16 {
        u128::MAX
    } else {
        (1u128 << (bytes as u32 * 8)) - 1
    }
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
                let z = apply_f32(f32::from_bits((a >> sh) as u32), f32::from_bits((b >> sh) as u32), op);
                r = (r & !(0xffff_ffffu128 << sh)) | ((z.to_bits() as u128) << sh);
            }
            FPrec::F64 => {
                let z = apply_f64(f64::from_bits((a >> sh) as u64), f64::from_bits((b >> sh) as u64), op);
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
    if diff < 0.5 {
        floor
    } else if diff > 0.5 {
        floor + 1.0
    } else if (floor as i64) & 1 == 0 {
        floor
    } else {
        floor + 1.0
    }
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
    let wide = (a as u128).wrapping_sub(b as u128).wrapping_sub(borrow_in as u128);
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

/// Result carrying only CF/OF (rotates leave the other flags untouched).
fn cf_of(res: u64, cf: bool, of: bool) -> AluResult {
    AluResult { res, cf, pf: false, af: false, zf: false, sf: false, of }
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
