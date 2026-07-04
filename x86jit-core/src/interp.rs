//! IR interpreter (§8.1). Walks an `IrBlock`'s ops over a `temps` vector and a
//! `&mut CpuState`, reading/writing shared guest `&Memory`. Slow but simple — the
//! oracle the JIT is validated against.
//!
//! RIP-on-trap convention (§8, §16), identical to the JIT's: on a memory trap RIP
//! is set to the FAULTING instruction (`cur_addr`, from `InsnStart`) so the user
//! can map/handle and retry; after `syscall`/`hlt` RIP is PAST the instruction.

use crate::exit::{AccessKind, Exit, StepResult};
use crate::ir::{Cond, FlagMask, IrBlock, IrOp, PackedBinOp, Val, VLogicOp};
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
            IrOp::VPackedShift { dst, a, imm, lane, right } => {
                cpu.xmm[*dst as usize] = packed_shift(cpu.xmm[*a as usize], *imm, *lane, *right);
            }
            IrOp::VByteShiftR { dst, a, bytes } => {
                let v = cpu.xmm[*a as usize];
                cpu.xmm[*dst as usize] = if *bytes >= 16 { 0 } else { v >> (*bytes as u32 * 8) };
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

/// Divide the `size`-width `hi:lo` dividend by `divisor` (§16). Returns the
/// (quotient, remainder), or `None` for `#DE` — a zero divisor or a quotient that
/// overflows the destination width. Shared by the interpreter and the JIT's div
/// helper so both agree exactly.
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
