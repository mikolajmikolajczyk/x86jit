//! Extracted `interpret_block` dispatch arm bodies (integer); see `super`.

use super::*;
use crate::ir::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_read_reg(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    reg: &crate::state::Reg,
) -> Option<StepResult> {
    temps[*dst as usize] = read_reg(cpu, *reg);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_write_reg(
    cpu: &mut CpuState,
    temps: &mut [u64],
    reg: &crate::state::Reg,
    src: &Val,
    size: &u8,
) -> Option<StepResult> {
    let v = read_val(*src, &*temps);
    write_reg(cpu, *reg, v, *size);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_add(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let r = alu_add(read_val(*a, &*temps), read_val(*b, &*temps), 0, *size);
    temps[*dst as usize] = r.res;
    apply(&mut cpu.flags, *set_flags, &r);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_adc(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let c = cpu.flags.cf as u64;
    let r = alu_add(read_val(*a, &*temps), read_val(*b, &*temps), c, *size);
    temps[*dst as usize] = r.res;
    apply(&mut cpu.flags, *set_flags, &r);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_sub(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let r = alu_sub(read_val(*a, &*temps), read_val(*b, &*temps), 0, *size);
    temps[*dst as usize] = r.res;
    apply(&mut cpu.flags, *set_flags, &r);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_sbb(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let c = cpu.flags.cf as u64;
    let r = alu_sub(read_val(*a, &*temps), read_val(*b, &*temps), c, *size);
    temps[*dst as usize] = r.res;
    apply(&mut cpu.flags, *set_flags, &r);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_and(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let r = alu_logic(read_val(*a, &*temps) & read_val(*b, &*temps), *size);
    temps[*dst as usize] = r.res;
    apply(&mut cpu.flags, *set_flags, &r);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_or(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let r = alu_logic(read_val(*a, &*temps) | read_val(*b, &*temps), *size);
    temps[*dst as usize] = r.res;
    apply(&mut cpu.flags, *set_flags, &r);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_xor(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let r = alu_logic(read_val(*a, &*temps) ^ read_val(*b, &*temps), *size);
    temps[*dst as usize] = r.res;
    apply(&mut cpu.flags, *set_flags, &r);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_shl(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let vm = read_val(*a, &*temps) & mask(*size);
    let cnt = read_val(*b, &*temps) & shift_mask(*size);
    let res = vm.wrapping_shl(cnt as u32) & mask(*size);
    temps[*dst as usize] = res;
    if !set_flags.is_none() && cnt != 0 {
        let n = (*size * 8) as u64;
        let cf = cnt <= n && (vm >> (n - cnt)) & 1 != 0;
        let of = (res & sign_bit(*size) != 0) ^ cf; // count==1 rule
        apply(
            &mut cpu.flags,
            *set_flags,
            &shift_result(res, *size, cf, of),
        );
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_shr(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let vm = read_val(*a, &*temps) & mask(*size);
    let cnt = read_val(*b, &*temps) & shift_mask(*size);
    let res = vm.wrapping_shr(cnt as u32);
    temps[*dst as usize] = res;
    if !set_flags.is_none() && cnt != 0 {
        let cf = (vm >> (cnt - 1)) & 1 != 0;
        let of = vm & sign_bit(*size) != 0; // count==1 rule: original MSB
        apply(
            &mut cpu.flags,
            *set_flags,
            &shift_result(res, *size, cf, of),
        );
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_double_shift(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    count: &Val,
    size: &u8,
    left: &bool,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let n = (*size * 8) as u64;
    let av = read_val(*a, &*temps) & mask(*size);
    let bv = read_val(*b, &*temps) & mask(*size);
    let cnt = read_val(*count, &*temps) & shift_mask(*size);
    if cnt == 0 {
        // Masked count 0 is a no-op; flags unchanged.
        temps[*dst as usize] = av;
    } else {
        let (res, cf) = if *left {
            let lo = av.wrapping_shl(cnt as u32);
            let hi = if cnt < n { bv >> (n - cnt) } else { 0 };
            let cf = cnt <= n && (av >> (n - cnt)) & 1 != 0;
            ((lo | hi) & mask(*size), cf)
        } else {
            let lo = av >> cnt;
            let hi = if cnt < n {
                bv.wrapping_shl((n - cnt) as u32)
            } else {
                0
            };
            let cf = (av >> (cnt - 1)) & 1 != 0;
            ((lo | hi) & mask(*size), cf)
        };
        temps[*dst as usize] = res;
        if !set_flags.is_none() {
            // OF (count==1): the result's sign bit flipped vs the source's.
            let of = (res & sign_bit(*size) != 0) ^ (av & sign_bit(*size) != 0);
            apply(
                &mut cpu.flags,
                *set_flags,
                &shift_result(res, *size, cf, of),
            );
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_sar(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let vm = read_val(*a, &*temps) & mask(*size);
    let cnt = read_val(*b, &*temps) & shift_mask(*size);
    let res = (sign_extend(vm, *size) as i64 >> cnt) as u64 & mask(*size);
    temps[*dst as usize] = res;
    if !set_flags.is_none() && cnt != 0 {
        // CF = last bit shifted out. For SAR the operand is sign-extended, so once the count
        // reaches the operand width the bit shifted out is the sign bit — use the sign-extended
        // value, not the width-masked `vm` (which would read 0 past its top bit). (task-270)
        let cf = (sign_extend(vm, *size) >> (cnt - 1)) & 1 != 0;
        apply(
            &mut cpu.flags,
            *set_flags,
            &shift_result(res, *size, cf, false),
        );
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_sext(temps: &mut [u64], dst: &Temp, a: &Val, from: &u8) -> Option<StepResult> {
    temps[*dst as usize] = sign_extend(read_val(*a, &*temps), *from);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_bswap(temps: &mut [u64], dst: &Temp, a: &Val, size: &u8) -> Option<StepResult> {
    let v = read_val(*a, &*temps);
    // `size` is the operand width: 8/4 for real `bswap`, 2 for a 16-bit `movbe`
    // (which reuses this op). The swap must be over exactly `size` bytes — a 16-bit
    // movbe needs a 2-byte swap, NOT the 32-bit swap the `else` used to force (that
    // left the stored low half zero — real hardware disagreed on `movbe [mem],r16`).
    temps[*dst as usize] = match *size {
        8 => v.swap_bytes(),
        2 => (v as u16).swap_bytes() as u64,
        _ => (v as u32).swap_bytes() as u64,
    };
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_rol(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let vm = read_val(*a, &*temps) & mask(*size);
    let cnt = read_val(*b, &*temps) & shift_mask(*size);
    let res = rotl(vm, cnt as u32, *size);
    temps[*dst as usize] = res;
    if !set_flags.is_none() && cnt != 0 {
        let cf = res & 1 != 0;
        let of = (res & sign_bit(*size) != 0) ^ cf; // count==1 rule
        apply(&mut cpu.flags, *set_flags, &cf_of(res, cf, of));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_ror(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let vm = read_val(*a, &*temps) & mask(*size);
    let cnt = read_val(*b, &*temps) & shift_mask(*size);
    let res = rotr(vm, cnt as u32, *size);
    temps[*dst as usize] = res;
    if !set_flags.is_none() && cnt != 0 {
        let n = *size * 8;
        let cf = res & sign_bit(*size) != 0;
        let of = cf ^ (res >> (n - 2) & 1 != 0); // top two bits differ
        apply(&mut cpu.flags, *set_flags, &cf_of(res, cf, of));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_rcl(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    // Rotate left through CF: a (size*8 + 1)-bit rotate including carry-in.
    let vm = read_val(*a, &*temps) & mask(*size);
    let bits = *size as u32 * 8;
    let cnt = (read_val(*b, &*temps) as u32 & shift_mask(*size) as u32) % (bits + 1);
    let (res, cf) = rcl(vm, cnt, cpu.flags.cf, *size);
    temps[*dst as usize] = res;
    if !set_flags.is_none() && cnt != 0 {
        // Left rotate: OF = CF-out XOR MSB(result) (defined for count 1).
        let of = cf ^ (res & sign_bit(*size) != 0);
        apply(&mut cpu.flags, *set_flags, &cf_of(res, cf, of));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_rcr(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    // Rotate right through CF (Go's div-by-constant carry fold, task-132).
    let vm = read_val(*a, &*temps) & mask(*size);
    let bits = *size as u32 * 8;
    let cnt = (read_val(*b, &*temps) as u32 & shift_mask(*size) as u32) % (bits + 1);
    let (res, cf) = rcr(vm, cnt, cpu.flags.cf, *size);
    temps[*dst as usize] = res;
    if !set_flags.is_none() && cnt != 0 {
        // Right rotate: OF = XOR of the top two result bits (defined for count 1).
        let n = *size * 8;
        let of = (res & sign_bit(*size) != 0) ^ (res >> (n - 2) & 1 != 0);
        apply(&mut cpu.flags, *set_flags, &cf_of(res, cf, of));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_mul(
    cpu: &mut CpuState,
    temps: &mut [u64],
    lo: &Temp,
    hi: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    signed: &bool,
    set_flags: &FlagMask,
) -> Option<StepResult> {
    let m = mask(*size);
    let n = *size * 8;
    let (va, vb) = (read_val(*a, &*temps) & m, read_val(*b, &*temps) & m);
    let (lo_v, hi_v, overflow) = if *signed {
        let p = sign_extend(va, *size) as i64 as i128 * sign_extend(vb, *size) as i64 as i128;
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
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_div(
    cpu: &mut CpuState,
    temps: &mut [u64],
    cur_addr: u64,
    quot: &Temp,
    rem: &Temp,
    hi: &Val,
    lo: &Val,
    divisor: &Val,
    size: &u8,
    signed: &bool,
) -> Option<StepResult> {
    let hv = read_val(*hi, &*temps);
    let lv = read_val(*lo, &*temps);
    let dv = read_val(*divisor, &*temps);
    match divide(hv, lv, dv, *size, *signed) {
        Some((q, r)) => {
            temps[*quot as usize] = q;
            temps[*rem as usize] = r;
        }
        // #DE: RIP on the faulting div; nothing committed to registers yet.
        None => {
            cpu.rip = cur_addr;
            return Some(StepResult::Exit(Exit::Exception {
                addr: cur_addr,
                vector: 0,
            }));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_get_cond(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    cond: &Cond,
) -> Option<StepResult> {
    temps[*dst as usize] = eval_cond(*cond, &cpu.flags) as u64;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_load(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &Temp,
    addr: &Val,
    size: &u8,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    // Resume after an MMIO read (§5.2): the block re-executes from the
    // faulting instruction, so this first load consumes the value the
    // embedder supplied via `complete_mmio_read` instead of re-trapping.
    if let Some(v) = cpu.pending_mmio.take() {
        temps[*dst as usize] = v & mask(*size);
    } else {
        match mem.read(a, *size) {
            Ok(v) => temps[*dst as usize] = v,
            Err(t) => return Some(trap_out(cpu, cur_addr, t, a, *size, AccessKind::Read, 0)),
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_store(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    addr: &Val,
    src: &Val,
    size: &u8,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let v = read_val(*src, &*temps);
    if let Err(t) = mem.write(a, v, *size) {
        // Resume after an MMIO write (§5.2): the block re-executes from
        // the faulting store. If the embedder acknowledged it via
        // `complete_mmio_write`, the side effect is already done — consume
        // the ack and continue instead of re-trapping.
        if t == MemTrap::Mmio && cpu.pending_mmio_write {
            cpu.pending_mmio_write = false;
        } else {
            return Some(trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, v));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_atomic_rmw(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    old: &Temp,
    addr: &Val,
    src: &Val,
    size: &u8,
    op: &RmwOp,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let s = read_val(*src, &*temps);
    match mem.atomic_rmw(a, s, *size, *op) {
        Ok(prev) => temps[*old as usize] = prev,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, s)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_atomic_cas(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    old: &Temp,
    addr: &Val,
    expected: &Val,
    src: &Val,
    size: &u8,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let (exp, s) = (read_val(*expected, &*temps), read_val(*src, &*temps));
    match mem.atomic_cas(a, exp, s, *size) {
        Ok(prev) => temps[*old as usize] = prev,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, s)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_bt(
    cpu: &mut CpuState,
    temps: &mut [u64],
    result: &Temp,
    a: &Val,
    bit: &Val,
    size: &u8,
    op: &BtOp,
) -> Option<StepResult> {
    let av = read_val(*a, &*temps);
    let b = read_val(*bit, &*temps) & (*size as u64 * 8 - 1);
    cpu.flags.cf = (av >> b) & 1 != 0;
    let m = 1u64 << b;
    let r = match op {
        BtOp::Test => av,
        BtOp::Set => av | m,
        BtOp::Reset => av & !m,
        BtOp::Complement => av ^ m,
    };
    temps[*result as usize] = r & mask(*size);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_cpuid(cpu: &mut CpuState) -> Option<StepResult> {
    cpuid_run(cpu);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_xgetbv(cpu: &mut CpuState) -> Option<StepResult> {
    xgetbv_run(cpu);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_popcnt(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    src: &Val,
    size: &u8,
) -> Option<StepResult> {
    let s = read_val(*src, &*temps) & mask(*size);
    temps[*dst as usize] = s.count_ones() as u64;
    cpu.flags.zf = s == 0;
    cpu.flags.cf = false;
    cpu.flags.of = false;
    cpu.flags.sf = false;
    cpu.flags.af = false;
    cpu.flags.pf = false;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_crc32(
    temps: &mut [u64],
    dst: &Temp,
    crc: &Val,
    src: &Val,
    bytes: &u8,
) -> Option<StepResult> {
    let c = read_val(*crc, &*temps) as u32;
    let s = read_val(*src, &*temps);
    temps[*dst as usize] = crc32c(c, s, *bytes) as u64;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_bmi(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    a: &Val,
    b: &Val,
    size: &u8,
    op: &BmiOp,
) -> Option<StepResult> {
    let av = read_val(*a, &*temps);
    let bv = read_val(*b, &*temps);
    let (r, cf) = bmi_result(av, bv, *size, *op);
    if op.writes_flags() {
        let bits = *size as u32 * 8;
        cpu.flags.cf = cf;
        cpu.flags.zf = r == 0;
        cpu.flags.sf = (r >> (bits - 1)) & 1 != 0;
        cpu.flags.of = false;
    }
    temps[*dst as usize] = r;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_bit_scan(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    src: &Val,
    old: &Val,
    size: &u8,
    op: &BitScanOp,
) -> Option<StepResult> {
    use crate::ir::BitScanOp::*;
    let bits = *size as u64 * 8;
    let s = read_val(*src, &*temps) & mask(*size);
    let r = match op {
        Bsf | Bsr if s == 0 => {
            // Destination preserved; only ZF set.
            cpu.flags.zf = true;
            read_val(*old, &*temps) & mask(*size)
        }
        Bsf => {
            cpu.flags.zf = false;
            s.trailing_zeros() as u64
        }
        Bsr => {
            cpu.flags.zf = false;
            63 - s.leading_zeros() as u64
        }
        Tzcnt => {
            let r = if s == 0 {
                bits
            } else {
                s.trailing_zeros() as u64
            };
            cpu.flags.cf = s == 0;
            cpu.flags.zf = r == 0;
            r
        }
        Lzcnt => {
            let r = s.leading_zeros() as u64 - (64 - bits);
            cpu.flags.cf = s == 0;
            cpu.flags.zf = r == 0;
            r
        }
    };
    temps[*dst as usize] = r;
    None
}

/// Decimal/ASCII accumulator adjust (§17.6): `daa`/`das`/`aaa`/`aas`/`aam`/`aad`,
/// following the 80286-defined flag behaviour. The flags each leaves *undefined* (OF
/// for daa/das; OF/SF/ZF/PF for aaa/aas; OF/AF/CF for aam/aad) are irrelevant to both
/// oracles — the 8088 corpus masks them off, and the Unicorn `MODE_16` differential
/// does not compare flags — so only the defined flags and the AL/AH/AX result are
/// pinned. `aam` with a zero base divides by zero and raises `#DE` (a fault: RIP stays
/// on the instruction, delivered in-guest through the IVT by `step_instruction`).
pub(crate) fn exec_bcd(cpu: &mut CpuState, cur_addr: u64, kind: &BcdKind) -> Option<StepResult> {
    let al = (cpu.gpr[RAX] & 0xFF) as u8;
    match kind {
        BcdKind::Daa | BcdKind::Das => {
            let sub = matches!(kind, BcdKind::Das);
            let old_al = al;
            let old_cf = cpu.flags.cf;
            let mut new_al = al;
            cpu.flags.cf = false;
            if (al & 0x0F) > 9 || cpu.flags.af {
                let (r, carry) = if sub {
                    new_al.overflowing_sub(6)
                } else {
                    new_al.overflowing_add(6)
                };
                new_al = r;
                cpu.flags.cf = old_cf || carry;
                cpu.flags.af = true;
            } else {
                cpu.flags.af = false;
            }
            if old_al > 0x99 || old_cf {
                new_al = if sub {
                    new_al.wrapping_sub(0x60)
                } else {
                    new_al.wrapping_add(0x60)
                };
                cpu.flags.cf = true;
            }
            cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xFF) | new_al as u64;
            cpu.flags.sf = new_al & 0x80 != 0;
            cpu.flags.zf = new_al == 0;
            cpu.flags.pf = parity(new_al as u64);
            cpu.flags.of = false;
        }
        BcdKind::Aaa | BcdKind::Aas => {
            // Intel: adjust ⇒ `AX ± 0x106` (the ±6 on AL carries into AH, plus the ±1 on
            // AH), then `AL &= 0x0F`. AAA adds, AAS subtracts.
            let mut ax = (cpu.gpr[RAX] & 0xFFFF) as u16;
            if (al & 0x0F) > 9 || cpu.flags.af {
                ax = if matches!(kind, BcdKind::Aas) {
                    ax.wrapping_sub(0x106)
                } else {
                    ax.wrapping_add(0x106)
                };
                cpu.flags.af = true;
                cpu.flags.cf = true;
            } else {
                cpu.flags.af = false;
                cpu.flags.cf = false;
            }
            ax &= 0xFF0F;
            cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xFFFF) | ax as u64;
        }
        BcdKind::Aam(base) => {
            if *base == 0 {
                // #DE (divide error): RIP on the faulting instruction (80286 fault).
                cpu.rip = cur_addr;
                return Some(StepResult::Exit(Exit::Exception {
                    addr: cur_addr,
                    vector: 0,
                }));
            }
            let ah = al / base;
            let new_al = al % base;
            cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xFFFF) | ((ah as u64) << 8) | new_al as u64;
            cpu.flags.sf = new_al & 0x80 != 0;
            cpu.flags.zf = new_al == 0;
            cpu.flags.pf = parity(new_al as u64);
        }
        BcdKind::Aad(base) => {
            let ah = ((cpu.gpr[RAX] >> 8) & 0xFF) as u8;
            let new_al = al.wrapping_add(ah.wrapping_mul(*base));
            cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xFFFF) | new_al as u64;
            cpu.flags.sf = new_al & 0x80 != 0;
            cpu.flags.zf = new_al == 0;
            cpu.flags.pf = parity(new_al as u64);
        }
    }
    None
}
