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
pub fn step_one(mem: &Memory, cpu: &mut CpuState, scratch: &mut Vec<u64>) -> StepResult {
    match crate::lift::lift_one(mem, cpu.rip) {
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

    for op in &ir.ops {
        match op {
            IrOp::InsnStart { guest_addr } => cur_addr = *guest_addr,

            IrOp::ReadReg { dst, reg } => temps[*dst as usize] = read_reg(cpu, *reg),
            IrOp::WriteReg { reg, src, size } => {
                let v = read_val(*src, &*temps);
                write_reg(cpu, *reg, v, *size);
            }

            IrOp::Add {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let r = alu_add(read_val(*a, &*temps), read_val(*b, &*temps), 0, *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Adc {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let c = cpu.flags.cf as u64;
                let r = alu_add(read_val(*a, &*temps), read_val(*b, &*temps), c, *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Sub {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let r = alu_sub(read_val(*a, &*temps), read_val(*b, &*temps), 0, *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Sbb {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let c = cpu.flags.cf as u64;
                let r = alu_sub(read_val(*a, &*temps), read_val(*b, &*temps), c, *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::And {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let r = alu_logic(read_val(*a, &*temps) & read_val(*b, &*temps), *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Or {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let r = alu_logic(read_val(*a, &*temps) | read_val(*b, &*temps), *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Xor {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let r = alu_logic(read_val(*a, &*temps) ^ read_val(*b, &*temps), *size);
                temps[*dst as usize] = r.res;
                apply(&mut cpu.flags, *set_flags, &r);
            }
            IrOp::Shl {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
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
            }
            IrOp::Shr {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
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
            }
            IrOp::Sar {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let vm = read_val(*a, &*temps) & mask(*size);
                let cnt = read_val(*b, &*temps) & shift_mask(*size);
                let res = (sign_extend(vm, *size) as i64 >> cnt) as u64 & mask(*size);
                temps[*dst as usize] = res;
                if !set_flags.is_none() && cnt != 0 {
                    let cf = (vm >> (cnt - 1)) & 1 != 0;
                    apply(
                        &mut cpu.flags,
                        *set_flags,
                        &shift_result(res, *size, cf, false),
                    );
                }
            }
            IrOp::Sext { dst, a, from } => {
                temps[*dst as usize] = sign_extend(read_val(*a, &*temps), *from);
            }
            IrOp::Bswap { dst, a, size } => {
                let v = read_val(*a, &*temps);
                temps[*dst as usize] = if *size == 8 {
                    v.swap_bytes()
                } else {
                    (v as u32).swap_bytes() as u64
                };
            }
            IrOp::Rol {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let vm = read_val(*a, &*temps) & mask(*size);
                let cnt = read_val(*b, &*temps) & shift_mask(*size);
                let res = rotl(vm, cnt as u32, *size);
                temps[*dst as usize] = res;
                if !set_flags.is_none() && cnt != 0 {
                    let cf = res & 1 != 0;
                    let of = (res & sign_bit(*size) != 0) ^ cf; // count==1 rule
                    apply(&mut cpu.flags, *set_flags, &cf_of(res, cf, of));
                }
            }
            IrOp::Ror {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
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
            }
            IrOp::Rcl {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
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
            }
            IrOp::Rcr {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
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
                let m = mask(*size);
                let n = *size * 8;
                let (va, vb) = (read_val(*a, &*temps) & m, read_val(*b, &*temps) & m);
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

            IrOp::Div {
                quot,
                rem,
                hi,
                lo,
                divisor,
                size,
                signed,
            } => {
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
                        return StepResult::Exit(Exit::Exception {
                            addr: cur_addr,
                            vector: 0,
                        });
                    }
                }
            }

            IrOp::GetCond { dst, cond } => {
                temps[*dst as usize] = eval_cond(*cond, &cpu.flags) as u64;
            }

            IrOp::Load { dst, addr, size } => {
                let a = read_val(*addr, &*temps);
                // Resume after an MMIO read (§5.2): the block re-executes from the
                // faulting instruction, so this first load consumes the value the
                // embedder supplied via `complete_mmio_read` instead of re-trapping.
                if let Some(v) = cpu.pending_mmio.take() {
                    temps[*dst as usize] = v & mask(*size);
                } else {
                    match mem.read(a, *size) {
                        Ok(v) => temps[*dst as usize] = v,
                        Err(t) => return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Read, 0),
                    }
                }
            }
            IrOp::Store {
                addr, src, size, ..
            } => {
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
                        return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, v);
                    }
                }
            }
            IrOp::AtomicRmw {
                old,
                addr,
                src,
                size,
                op,
            } => {
                let a = read_val(*addr, &*temps);
                let s = read_val(*src, &*temps);
                match mem.atomic_rmw(a, s, *size, *op) {
                    Ok(prev) => temps[*old as usize] = prev,
                    Err(t) => return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, s),
                }
            }
            IrOp::AtomicCas {
                old,
                addr,
                expected,
                src,
                size,
            } => {
                let a = read_val(*addr, &*temps);
                let (exp, s) = (read_val(*expected, &*temps), read_val(*src, &*temps));
                match mem.atomic_cas(a, exp, s, *size) {
                    Ok(prev) => temps[*old as usize] = prev,
                    Err(t) => return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, s),
                }
            }
            IrOp::Bt {
                result,
                a,
                bit,
                size,
                op,
            } => {
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
            }
            IrOp::Cpuid => cpuid_run(cpu),
            IrOp::Xgetbv => xgetbv_run(cpu),
            IrOp::X87 { kind, addr, sti } => {
                let a = read_val(*addr, &*temps);
                // Through `Memory`: RAM region check + SMC `note_write` on stores, so
                // a self-modifying x87 store invalidates like a scalar `Store` (§10).
                if let Some((fault, write)) = crate::x87::exec_x87(cpu, mem, *kind, a, *sti) {
                    let access = if write {
                        AccessKind::Write
                    } else {
                        AccessKind::Read
                    };
                    // RIP already on the faulting instruction (cur_addr) via InsnStart.
                    cpu.rip = cur_addr;
                    return StepResult::Exit(Exit::UnmappedMemory {
                        addr: fault,
                        access,
                    });
                }
            }
            IrOp::FxState { addr, restore } => {
                let a = read_val(*addr, &*temps);
                // Through `Memory` (RAM check + SMC note_write), like the x87 arm.
                if let Some((fault, write)) = crate::x87::exec_fxstate(cpu, mem, a, *restore) {
                    cpu.rip = cur_addr;
                    return StepResult::Exit(Exit::UnmappedMemory {
                        addr: fault,
                        access: if write {
                            AccessKind::Write
                        } else {
                            AccessKind::Read
                        },
                    });
                }
            }
            IrOp::Popcnt { dst, src, size } => {
                let s = read_val(*src, &*temps) & mask(*size);
                temps[*dst as usize] = s.count_ones() as u64;
                cpu.flags.zf = s == 0;
                cpu.flags.cf = false;
                cpu.flags.of = false;
                cpu.flags.sf = false;
                cpu.flags.af = false;
                cpu.flags.pf = false;
            }
            IrOp::Crc32 {
                dst,
                crc,
                src,
                bytes,
            } => {
                let c = read_val(*crc, &*temps) as u32;
                let s = read_val(*src, &*temps);
                temps[*dst as usize] = crc32c(c, s, *bytes) as u64;
            }
            IrOp::Bmi {
                dst,
                a,
                b,
                size,
                op,
            } => {
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
            }
            IrOp::BitScan {
                dst,
                src,
                old,
                size,
                op,
            } => {
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
            }

            IrOp::VLoad { dst, addr, size } => {
                let a = read_val(*addr, &*temps);
                match vload(mem, a, *size) {
                    Ok(v) => cpu.xmm[*dst as usize] = v,
                    Err(t) => return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Read, 0),
                }
            }
            IrOp::VStore { addr, src, size } => {
                let a = read_val(*addr, &*temps);
                let v = cpu.xmm[*src as usize];
                if let Err(t) = vstore(mem, a, v, *size) {
                    return trap_out(cpu, cur_addr, t, a, *size, AccessKind::Write, v as u64);
                }
            }
            IrOp::VMov { dst, src } => cpu.xmm[*dst as usize] = cpu.xmm[*src as usize],
            IrOp::VLoadWide { dst, addr, bytes } => {
                let a = read_val(*addr, &*temps);
                let mut lanes = [0u128; 4];
                // Load `bytes/16` 128-bit lanes; set_vec zero-extends above `bytes`.
                for (i, slot) in lanes.iter_mut().enumerate().take(*bytes as usize / 16) {
                    let ea = a.wrapping_add(i as u64 * 16);
                    match vload(mem, ea, 16) {
                        Ok(v) => *slot = v,
                        Err(t) => return trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0),
                    }
                }
                cpu.set_vec(*dst as usize, lanes, *bytes);
            }
            IrOp::VStoreWide { addr, src, bytes } => {
                let a = read_val(*addr, &*temps);
                let lanes = cpu.vec_lanes(*src as usize);
                for (i, v) in lanes.into_iter().enumerate().take(*bytes as usize / 16) {
                    let ea = a.wrapping_add(i as u64 * 16);
                    if let Err(t) = vstore(mem, ea, v, 16) {
                        return trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Write, v as u64);
                    }
                }
            }
            IrOp::VMovWide { dst, src, bytes } => {
                let lanes = cpu.vec_lanes(*src as usize);
                cpu.set_vec(*dst as usize, lanes, *bytes);
            }
            IrOp::VMaskMov {
                dst,
                src,
                k,
                elem,
                zeroing,
                bytes,
            } => {
                let newval = cpu.vec_lanes(*src as usize);
                cpu.write_masked(*dst as usize, newval, *k, *elem, *zeroing, *bytes);
            }
            IrOp::VLogic256 { dst, a, b, op } => {
                cpu.xmm[*dst as usize] = vlogic(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *op);
                cpu.ymm_hi[*dst as usize] =
                    vlogic(cpu.ymm_hi[*a as usize], cpu.ymm_hi[*b as usize], *op);
            }
            IrOp::VLogicWide {
                dst,
                a,
                b,
                op,
                bytes,
            } => {
                let (al, bl) = (cpu.vec_lanes(*a as usize), cpu.vec_lanes(*b as usize));
                let mut r = [0u128; 4];
                for i in 0..4 {
                    r[i] = vlogic(al[i], bl[i], *op);
                }
                cpu.set_vec(*dst as usize, r, *bytes);
            }
            IrOp::VPMovExtend {
                dst,
                src,
                from,
                to,
                signed,
            } => {
                cpu.xmm[*dst as usize] = pmov_extend(cpu.xmm[*src as usize], *from, *to, *signed);
            }
            IrOp::VPMovExtendM {
                dst,
                addr,
                from,
                to,
                signed,
            } => {
                let nbytes = (16 / *to as usize) * *from as usize;
                let av = read_val(*addr, &*temps);
                match vload(mem, av, nbytes as u8) {
                    Ok(m) => cpu.xmm[*dst as usize] = pmov_extend(m, *from, *to, *signed),
                    Err(t) => {
                        return trap_out(cpu, cur_addr, t, av, nbytes as u8, AccessKind::Read, 0)
                    }
                }
            }
            IrOp::VPBlendV { dst, src, lane } => {
                let (d, s, m) = (cpu.xmm[*dst as usize], cpu.xmm[*src as usize], cpu.xmm[0]);
                cpu.xmm[*dst as usize] = blendv(d, s, m, *lane);
            }
            IrOp::VPBlendVM { dst, addr, lane } => {
                let av = read_val(*addr, &*temps);
                let (d, m) = (cpu.xmm[*dst as usize], cpu.xmm[0]);
                match vload(mem, av, 16) {
                    Ok(s) => cpu.xmm[*dst as usize] = blendv(d, s, m, *lane),
                    Err(t) => return trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0),
                }
            }
            IrOp::VPRound {
                dst,
                src,
                prec,
                mode,
                scalar,
            } => {
                let (d, s) = (cpu.xmm[*dst as usize], cpu.xmm[*src as usize]);
                cpu.xmm[*dst as usize] = vround(d, s, *prec, *mode, *scalar);
            }
            IrOp::VPRoundM {
                dst,
                addr,
                prec,
                mode,
                scalar,
            } => {
                let av = read_val(*addr, &*temps);
                // Packed loads 16 bytes; scalar loads only one element.
                let size = if *scalar { prec.bytes() } else { 16 };
                let d = cpu.xmm[*dst as usize];
                match vload(mem, av, size) {
                    Ok(s) => cpu.xmm[*dst as usize] = vround(d, s, *prec, *mode, *scalar),
                    Err(t) => return trap_out(cpu, cur_addr, t, av, size, AccessKind::Read, 0),
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
                apply_masked_logic(cpu, *op, *dst, *a, *b, *k, *elem, *zeroing, *bytes);
            }
            IrOp::VInsertLaneWide {
                dst,
                src,
                ins,
                idx,
                num_lanes,
                bytes,
            } => {
                let mut lanes = cpu.vec_lanes(*src as usize);
                let inl = cpu.vec_lanes(*ins as usize);
                let base = *idx as usize * *num_lanes as usize;
                let n = *num_lanes as usize;
                lanes[base..base + n].copy_from_slice(&inl[..n]);
                cpu.set_vec(*dst as usize, lanes, *bytes);
            }
            IrOp::VPcmpStr {
                a,
                b,
                imm,
                explicit,
            } => {
                let (ecx, cf, zf, sf, of) = pcmpstr_run(cpu, *a, *b, *imm, *explicit);
                cpu.write_gpr(1, ecx as u64, 4); // ECX (zero-extends RCX)
                cpu.flags.cf = cf;
                cpu.flags.zf = zf;
                cpu.flags.sf = sf;
                cpu.flags.of = of;
                cpu.flags.af = false;
                cpu.flags.pf = false;
            }
            IrOp::VAlign {
                dst,
                a,
                b,
                shift,
                elem,
                bytes,
            } => {
                cpu.set_vec(
                    *dst as usize,
                    valign_lanes(
                        cpu.vec_lanes(*a as usize),
                        cpu.vec_lanes(*b as usize),
                        *shift,
                        *elem,
                        *bytes,
                    ),
                    *bytes,
                );
            }
            IrOp::VPTernlog {
                dst,
                b,
                c,
                imm,
                bytes,
            } => {
                let al = cpu.vec_lanes(*dst as usize); // dst is also the first source
                let (bl, cl) = (cpu.vec_lanes(*b as usize), cpu.vec_lanes(*c as usize));
                let mut r = [0u128; 4];
                for i in 0..4 {
                    r[i] = ternlog(al[i], bl[i], cl[i], *imm);
                }
                cpu.set_vec(*dst as usize, r, *bytes);
            }
            IrOp::VLogic256M { dst, a, addr, op } => {
                let av = read_val(*addr, &*temps);
                let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
                match vload(mem, av, 16) {
                    Ok(m) => cpu.xmm[*dst as usize] = vlogic(alo, m, *op),
                    Err(t) => return trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0),
                }
                let hi = av.wrapping_add(16);
                match vload(mem, hi, 16) {
                    Ok(m) => cpu.ymm_hi[*dst as usize] = vlogic(ahi, m, *op),
                    Err(t) => return trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0),
                }
            }
            IrOp::VPackedBin256 {
                dst,
                a,
                b,
                lane,
                op,
            } => {
                cpu.xmm[*dst as usize] =
                    packed_bin(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *lane, *op);
                cpu.ymm_hi[*dst as usize] =
                    packed_bin(cpu.ymm_hi[*a as usize], cpu.ymm_hi[*b as usize], *lane, *op);
            }
            IrOp::VPackedBin256M {
                dst,
                a,
                addr,
                lane,
                op,
            } => {
                let av = read_val(*addr, &*temps);
                let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
                match vload(mem, av, 16) {
                    Ok(m) => cpu.xmm[*dst as usize] = packed_bin(alo, m, *lane, *op),
                    Err(t) => return trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0),
                }
                let hi = av.wrapping_add(16);
                match vload(mem, hi, 16) {
                    Ok(m) => cpu.ymm_hi[*dst as usize] = packed_bin(ahi, m, *lane, *op),
                    Err(t) => return trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0),
                }
            }
            IrOp::VMoveMaskB256 { dst, src } => {
                let (lo, hi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
                temps[*dst as usize] = movemask_b(lo) | (movemask_b(hi) << 16);
            }
            IrOp::VFromGpr { dst, src, size } => {
                let v = read_val(*src, &*temps) & mask(*size);
                cpu.xmm[*dst as usize] = v as u128;
            }
            IrOp::VToGpr { dst, src, size } => {
                temps[*dst as usize] = (cpu.xmm[*src as usize] as u64) & mask(*size);
            }
            IrOp::VLogic { dst, a, b, op } => {
                let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                cpu.xmm[*dst as usize] = vlogic(va, vb, *op);
            }
            IrOp::VPackedBin {
                dst,
                a,
                b,
                lane,
                op,
            } => {
                let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                cpu.xmm[*dst as usize] = packed_bin(va, vb, *lane, *op);
            }
            IrOp::VPackedBinM {
                dst,
                addr,
                lane,
                op,
            } => {
                let a = read_val(*addr, &*temps);
                match vload(mem, a, 16) {
                    Ok(bv) => {
                        cpu.xmm[*dst as usize] = packed_bin(cpu.xmm[*dst as usize], bv, *lane, *op)
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0),
                }
            }
            IrOp::VLogicM { dst, addr, op } => {
                let a = read_val(*addr, &*temps);
                match vload(mem, a, 16) {
                    Ok(bv) => {
                        cpu.xmm[*dst as usize] = vlogic(cpu.xmm[*dst as usize], bv, *op);
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0),
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
                cpu.xmm[*dst as usize] =
                    packed_shift(cpu.xmm[*a as usize], *imm, *lane, *right, *arith);
            }
            IrOp::VByteShift {
                dst,
                a,
                bytes,
                right,
            } => {
                let v = cpu.xmm[*a as usize];
                cpu.xmm[*dst as usize] = if *bytes >= 16 {
                    0
                } else if *right {
                    v >> (*bytes as u32 * 8)
                } else {
                    v << (*bytes as u32 * 8)
                };
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
            IrOp::VMoveHalf {
                dst,
                src,
                dst_high,
                src_high,
            } => {
                let s = cpu.xmm[*src as usize];
                let half = if *src_high {
                    s >> 64
                } else {
                    s & 0xffff_ffff_ffff_ffffu128
                };
                let d = cpu.xmm[*dst as usize];
                cpu.xmm[*dst as usize] = if *dst_high {
                    (d & 0xffff_ffff_ffff_ffffu128) | (half << 64)
                } else {
                    (d & !0xffff_ffff_ffff_ffffu128) | half
                };
            }
            IrOp::VLoadHalf { dst, addr, high } => {
                let a = read_val(*addr, &*temps);
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
                let a = read_val(*addr, &*temps);
                let s = cpu.xmm[*src as usize];
                let half = if *high {
                    s >> 64
                } else {
                    s & 0xffff_ffff_ffff_ffffu128
                };
                if let Err(t) = vstore(mem, a, half, 8) {
                    return trap_out(cpu, cur_addr, t, a, 8, AccessKind::Write, half as u64);
                }
            }
            IrOp::VExtractW { dst, src, index } => {
                let sh = (*index as u32 & 7) * 16;
                temps[*dst as usize] = ((cpu.xmm[*src as usize] >> sh) & 0xffff) as u64;
            }
            IrOp::VExtractLane {
                dst,
                src,
                index,
                size,
            } => {
                let bits = *size as u32 * 8;
                let sh = (*index as u32 % (128 / bits)) * bits;
                let mask = lane_mask(*size);
                temps[*dst as usize] = ((cpu.xmm[*src as usize] >> sh) & mask) as u64;
            }
            IrOp::VMoveMaskB { dst, src } => {
                temps[*dst as usize] = movemask_b(cpu.xmm[*src as usize]);
            }
            IrOp::VBroadcast {
                dst,
                src,
                elem,
                w256,
            } => {
                let v = broadcast_elem(cpu.xmm[*src as usize], *elem);
                cpu.xmm[*dst as usize] = v;
                cpu.ymm_hi[*dst as usize] = if *w256 { v } else { 0 };
            }
            IrOp::VBroadcastM {
                dst,
                addr,
                elem,
                w256,
            } => {
                let a = read_val(*addr, &*temps);
                match mem.read(a, *elem) {
                    Ok(e) => {
                        let v = broadcast_elem(e as u128, *elem);
                        cpu.xmm[*dst as usize] = v;
                        cpu.ymm_hi[*dst as usize] = if *w256 { v } else { 0 };
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, a, *elem, AccessKind::Read, 0),
                }
            }
            IrOp::VBroadcastGpr {
                dst,
                src,
                elem,
                width,
            } => {
                let v = broadcast_elem(read_val(*src, &*temps) as u128, *elem);
                cpu.set_vec(*dst as usize, [v; 4], *width);
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
                let av = cpu.vec_lanes(*a as usize);
                let bv = cpu.vec_lanes(*b as usize);
                let mut m = vpcmp_mask(av, bv, *elem, *width, *pred, *signed);
                if let Some(wk) = writemask {
                    m &= cpu.kmask[*wk as usize];
                }
                cpu.kmask[*k as usize] = m;
            }
            IrOp::VKOrTest { a, b, width } => {
                let wmask = kwidth_mask(*width);
                let t = (cpu.kmask[*a as usize] | cpu.kmask[*b as usize]) & wmask;
                cpu.flags.zf = t == 0;
                cpu.flags.cf = t == wmask;
                cpu.flags.of = false;
                cpu.flags.sf = false;
                cpu.flags.af = false;
                cpu.flags.pf = false;
            }
            IrOp::VKFromGpr { k, src, width } => {
                cpu.kmask[*k as usize] = read_val(*src, &*temps) & kwidth_mask(*width);
            }
            IrOp::VKToGpr { dst, k, width } => {
                temps[*dst as usize] = cpu.kmask[*k as usize] & kwidth_mask(*width);
            }
            IrOp::VKMovKK { dst, src, width } => {
                cpu.kmask[*dst as usize] = cpu.kmask[*src as usize] & kwidth_mask(*width);
            }
            IrOp::VInsert128 { dst, src, ins, hi } => {
                let (slo, shi, insv) = (
                    cpu.xmm[*src as usize],
                    cpu.ymm_hi[*src as usize],
                    cpu.xmm[*ins as usize],
                );
                if *hi {
                    cpu.xmm[*dst as usize] = slo;
                    cpu.ymm_hi[*dst as usize] = insv;
                } else {
                    cpu.xmm[*dst as usize] = insv;
                    cpu.ymm_hi[*dst as usize] = shi;
                }
            }
            IrOp::VExtract128 { dst, src, hi } => {
                let v = if *hi {
                    cpu.ymm_hi[*src as usize]
                } else {
                    cpu.xmm[*src as usize]
                };
                cpu.xmm[*dst as usize] = v;
                cpu.ymm_hi[*dst as usize] = 0; // XMM destination (VEX) zeroes the upper
            }
            IrOp::VPshufb256 { dst, a, idx } => {
                let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
                let (ilo, ihi) = (cpu.xmm[*idx as usize], cpu.ymm_hi[*idx as usize]);
                cpu.xmm[*dst as usize] = pshufb(alo, ilo);
                cpu.ymm_hi[*dst as usize] = pshufb(ahi, ihi);
            }
            IrOp::VPshufb256M { dst, a, addr } => {
                let av = read_val(*addr, &*temps);
                let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
                match vload(mem, av, 16) {
                    Ok(ilo) => cpu.xmm[*dst as usize] = pshufb(alo, ilo),
                    Err(t) => return trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0),
                }
                let hi = av.wrapping_add(16);
                match vload(mem, hi, 16) {
                    Ok(ihi) => cpu.ymm_hi[*dst as usize] = pshufb(ahi, ihi),
                    Err(t) => return trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0),
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
                cpu.xmm[*dst as usize] =
                    packed_shift(cpu.xmm[*a as usize], *imm, *lane, *right, *arith);
                cpu.ymm_hi[*dst as usize] =
                    packed_shift(cpu.ymm_hi[*a as usize], *imm, *lane, *right, *arith);
            }
            IrOp::VPermq { dst, src, imm } => {
                let (lo, hi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
                let q = [lo as u64, (lo >> 64) as u64, hi as u64, (hi >> 64) as u64];
                let sel = |i: u32| q[((*imm >> (2 * i)) & 3) as usize] as u128;
                cpu.xmm[*dst as usize] = sel(0) | (sel(1) << 64);
                cpu.ymm_hi[*dst as usize] = sel(2) | (sel(3) << 64);
            }
            IrOp::VPermd { dst, ctrl, src } => {
                let (clo, chi) = (cpu.xmm[*ctrl as usize], cpu.ymm_hi[*ctrl as usize]);
                let (slo, shi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
                let dword = |v: (u128, u128), i: usize| -> u64 {
                    let w = if i < 4 { v.0 } else { v.1 };
                    ((w >> ((i % 4) * 32)) & 0xffff_ffff) as u64
                };
                let mut lo = 0u128;
                let mut hi = 0u128;
                for i in 0..8usize {
                    let idx = (dword((clo, chi), i) & 7) as usize;
                    let e = dword((slo, shi), idx) as u128;
                    if i < 4 {
                        lo |= e << (i * 32);
                    } else {
                        hi |= e << ((i - 4) * 32);
                    }
                }
                cpu.xmm[*dst as usize] = lo;
                cpu.ymm_hi[*dst as usize] = hi;
            }
            IrOp::VPerm2i128 { dst, a, b, imm } => {
                let halves = [
                    cpu.xmm[*a as usize],
                    cpu.ymm_hi[*a as usize],
                    cpu.xmm[*b as usize],
                    cpu.ymm_hi[*b as usize],
                ];
                let lane = |sel: u8| -> u128 {
                    if sel & 0x08 != 0 {
                        0
                    } else {
                        halves[(sel & 3) as usize]
                    }
                };
                cpu.xmm[*dst as usize] = lane(*imm);
                cpu.ymm_hi[*dst as usize] = lane(*imm >> 4);
            }
            IrOp::VPalignr256 { dst, a, b, imm } => {
                cpu.xmm[*dst as usize] = palignr(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *imm);
                cpu.ymm_hi[*dst as usize] =
                    palignr(cpu.ymm_hi[*a as usize], cpu.ymm_hi[*b as usize], *imm);
            }
            IrOp::VPtest { a, b, w256 } => {
                let (alo, blo) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                let (ahi, bhi) = if *w256 {
                    (cpu.ymm_hi[*a as usize], cpu.ymm_hi[*b as usize])
                } else {
                    (0, 0)
                };
                cpu.flags.zf = (blo & alo) == 0 && (bhi & ahi) == 0;
                cpu.flags.cf = (blo & !alo) == 0 && (bhi & !ahi) == 0;
                cpu.flags.of = false;
                cpu.flags.sf = false;
                cpu.flags.af = false;
                cpu.flags.pf = false;
            }
            IrOp::VZeroUpper { reg } => {
                cpu.ymm_hi[*reg as usize] = 0;
                cpu.zmm_hi[*reg as usize] = [0; 2]; // a 128-bit write clears bits 511:128
            }
            IrOp::VZeroUpperAll => {
                // vzeroupper/vzeroall zero bits 511:128 of ZMM0–15 (16–31 unaffected).
                cpu.ymm_hi[..16].fill(0);
                cpu.zmm_hi[..16].fill([0; 2]);
            }
            IrOp::VPshufb { dst, idx } => {
                cpu.xmm[*dst as usize] = pshufb(cpu.xmm[*dst as usize], cpu.xmm[*idx as usize]);
            }
            IrOp::VPshufbM { dst, addr } => {
                let a = read_val(*addr, &*temps);
                match vload(mem, a, 16) {
                    Ok(iv) => cpu.xmm[*dst as usize] = pshufb(cpu.xmm[*dst as usize], iv),
                    Err(t) => return trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0),
                }
            }
            IrOp::VAlignr { dst, src, imm } => {
                cpu.xmm[*dst as usize] =
                    palignr(cpu.xmm[*dst as usize], cpu.xmm[*src as usize], *imm);
            }
            IrOp::VAlignrM { dst, addr, imm } => {
                let a = read_val(*addr, &*temps);
                match vload(mem, a, 16) {
                    Ok(iv) => cpu.xmm[*dst as usize] = palignr(cpu.xmm[*dst as usize], iv, *imm),
                    Err(t) => return trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0),
                }
            }
            IrOp::VShufps { dst, a, b, imm } => {
                let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                let mut r = 0u128;
                for i in 0..4 {
                    let sel = (imm >> (2 * i)) & 3;
                    let src = if i < 2 { va } else { vb };
                    let lane = (src >> (sel as u32 * 32)) & 0xffff_ffff;
                    r |= lane << (i as u32 * 32);
                }
                cpu.xmm[*dst as usize] = r;
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
            IrOp::VUnpackLow {
                dst,
                a,
                b,
                lane,
                high,
            } => {
                cpu.xmm[*dst as usize] =
                    unpack_low(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *lane, *high);
            }
            IrOp::VPackUsWB { dst, a, b } => {
                cpu.xmm[*dst as usize] = packuswb(cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
            }
            IrOp::SetDf { value } => cpu.flags.df = *value,
            IrOp::RepString { op, elem, rep } => {
                // Route every element through `Memory` (region check + SMC `note_write`),
                // exactly like a scalar `Store` — so `rep stos` onto a code page is
                // caught and an MMIO/unmapped target traps (§10).
                if let Some(f) = string_run(cpu, mem, *op, *elem, *rep, cur_addr) {
                    let access = if f.write {
                        AccessKind::Write
                    } else {
                        AccessKind::Read
                    };
                    // `string_run` already set RIP to the faulting instruction.
                    let exit = match (f.trap, access) {
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
                    };
                    return StepResult::Exit(exit);
                }
            }
            IrOp::VInsertW { dst, src, index } => {
                let v = read_val(*src, &*temps) as u16 as u128;
                let sh = (*index as u32 & 7) * 16;
                let old = cpu.xmm[*dst as usize];
                cpu.xmm[*dst as usize] = (old & !(0xffffu128 << sh)) | (v << sh);
            }
            IrOp::VInsertLane {
                dst,
                base,
                src,
                index,
                size,
            } => {
                let bits = *size as u32 * 8;
                let lane_mask = lane_mask(*size);
                let v = (read_val(*src, &*temps) as u128) & lane_mask;
                let sh = (*index as u32 % (128 / bits)) * bits;
                let old = cpu.xmm[*base as usize];
                cpu.xmm[*dst as usize] = (old & !(lane_mask << sh)) | (v << sh);
            }
            IrOp::VFloatMov { dst, src, prec } => {
                let m = lane_mask(prec.bytes());
                let s = cpu.xmm[*src as usize] & m;
                cpu.xmm[*dst as usize] = (cpu.xmm[*dst as usize] & !m) | s;
            }
            IrOp::VFloatBin {
                dst,
                a,
                b,
                op,
                prec,
                scalar,
            } => {
                let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                cpu.xmm[*dst as usize] = float_bin(va, vb, *op, *prec, *scalar);
            }
            IrOp::VFloatBinM {
                dst,
                addr,
                op,
                prec,
                scalar,
            } => {
                let a = read_val(*addr, &*temps);
                let size = if *scalar { prec.bytes() } else { 16 };
                match vload(mem, a, size) {
                    Ok(bv) => {
                        cpu.xmm[*dst as usize] =
                            float_bin(cpu.xmm[*dst as usize], bv, *op, *prec, *scalar)
                    }
                    Err(t) => return trap_out(cpu, cur_addr, t, a, size, AccessKind::Read, 0),
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
                let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
                cpu.xmm[*dst as usize] =
                    float_cmp_mask(cpu.xmm[*dst as usize], va, vb, *prec, *scalar, *pred);
            }
            IrOp::VFloatCmp { a, b, prec } => {
                let (zf, pf, cf) =
                    float_compare(read_val(*a, &*temps), read_val(*b, &*temps), *prec);
                cpu.flags.zf = zf;
                cpu.flags.pf = pf;
                cpu.flags.cf = cf;
                cpu.flags.of = false;
                cpu.flags.sf = false;
                cpu.flags.af = false;
            }
            IrOp::VCvtFromInt {
                dst,
                src,
                int_size,
                prec,
            } => {
                let signed = sign_extend(read_val(*src, &*temps), *int_size) as i64;
                let bits = match prec {
                    FPrec::F32 => (signed as f32).to_bits() as u128,
                    FPrec::F64 => (signed as f64).to_bits() as u128,
                };
                let m = lane_mask(prec.bytes());
                cpu.xmm[*dst as usize] = (cpu.xmm[*dst as usize] & !m) | (bits & m);
            }
            IrOp::VCvtToInt {
                dst,
                src,
                int_size,
                prec,
                trunc,
            } => {
                let raw = read_val(*src, &*temps);
                let f = match prec {
                    FPrec::F32 => f32::from_bits(raw as u32) as f64,
                    FPrec::F64 => f64::from_bits(raw),
                };
                let f = if *trunc {
                    f.trunc()
                } else {
                    round_ties_even(f)
                };
                // Saturating cast to the destination width (Rust `as` clamps to
                // INT_MIN/MAX); matches the JIT's `fcvt_to_sint_sat`. The x86
                // integer-indefinite result on invalid operands is deferred.
                temps[*dst as usize] = match int_size {
                    8 => f as i64 as u64,
                    _ => f as i32 as u32 as u64,
                };
            }
            IrOp::VCvtFloat { dst, src, from, to } => {
                let raw = read_val(*src, &*temps);
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
            IrOp::VFloatUnary {
                dst,
                src,
                op,
                prec,
                scalar,
            } => {
                cpu.xmm[*dst as usize] = float_unary(
                    cpu.xmm[*dst as usize],
                    cpu.xmm[*src as usize],
                    *op,
                    *prec,
                    *scalar,
                );
            }

            IrOp::Jump { target } => {
                cpu.rip = read_val(*target, &*temps);
                return StepResult::Continue;
            }
            IrOp::Branch {
                cond,
                taken,
                fallthrough,
            } => {
                cpu.rip = if eval_cond(*cond, &cpu.flags) {
                    *taken
                } else {
                    *fallthrough
                };
                return StepResult::Continue;
            }
            IrOp::Call {
                target,
                return_addr,
            } => {
                let sp = cpu.gpr[RSP].wrapping_sub(8);
                if let Err(t) = mem.write(sp, *return_addr, 8) {
                    return trap_out(cpu, cur_addr, t, sp, 8, AccessKind::Write, *return_addr);
                }
                cpu.gpr[RSP] = sp;
                cpu.rip = read_val(*target, &*temps);
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
    let (av, bv) = (cpu.xmm[a as usize], cpu.xmm[b as usize]);
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
            PackedBinOp::MulLo32 => la.wrapping_mul(lb) & lane_mask,
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
        // Structured extended features (subleaf 0): AVX2 / BMI / AVX-512 in EBX.
        0x7 => (0, f.leaf7_ebx(), 0, 0),
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
