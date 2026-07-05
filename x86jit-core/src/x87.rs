//! x87 FPU (§14), backed by `f64` rather than the architectural 80-bit extended
//! precision. One `exec_x87` routine drives both backends (the interpreter calls
//! it directly; the JIT via a helper), so they agree bit-for-bit with each other.
//! Values that originate as double/float/int round-trip exactly; the extra 11
//! mantissa bits of true `long double` are lost, so raw `printf("%Lf")` output can
//! differ in the last digits — fine for arithmetic and comparison-driven code.
//!
//! The register file is a stack: `ST(i)` = `fpr[(fpu_top + i) & 7]`. `fld`-style
//! ops decrement `fpu_top` then write `ST(0)`; `fstp`-style ops read `ST(0)` then
//! increment. Memory operands are read/written through a raw guest pointer with a
//! bounds check; a fault returns `Some((addr, is_write))` so the caller traps with
//! RIP on the instruction (§8, §16), exactly like the string helper.

use crate::state::CpuState;

/// One x87 operation. Memory forms carry their access in `addr`/size via the op
/// variant; register/stack forms use the `sti` argument to `exec_x87`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum FpuKind {
    // memory load, push ST(0)
    FldF64,
    FldF32,
    FldF80,
    FildI16,
    FildI32,
    FildI64,
    // memory store from ST(0); the `P` forms pop
    FstpF64,
    FstpF32,
    FstpF80,
    FistpI16,
    FistpI32,
    FistpI64,
    FstF64,
    FstF32,
    // ST(0) op= memory
    FaddMemF64,
    FaddMemF32,
    FsubMemF64,
    FsubMemF32,
    FsubrMemF64,
    FsubrMemF32,
    FmulMemF64,
    FmulMemF32,
    FdivMemF64,
    FdivMemF32,
    FdivrMemF64,
    FdivrMemF32,
    // push a copy of ST(i)
    FldSti,
    // register/stack store: ST(i) = ST(0); the `p` form pops
    FstSti,
    FstpSti,
    // ST(0) op= ST(i) (register forms, ST(0) destination, no pop)
    FsubSti,  // ST(0) -= ST(i)
    FsubrSti, // ST(0) = ST(i) - ST(0)
    FdivSti,  // ST(0) /= ST(i)
    FdivrSti, // ST(0) = ST(i) / ST(0)
    // ST(i) op= ST(0) (register forms, ST(i) destination, no pop) — the `p` forms
    // above with the pop removed (e.g. `fmul st(1), st(0)`).
    FaddToSti,  // ST(i) += ST(0)
    FsubToSti,  // ST(i) -= ST(0)
    FsubrToSti, // ST(i) = ST(0) - ST(i)
    FmulToSti,  // ST(i) *= ST(0)
    FdivToSti,  // ST(i) /= ST(0)
    FdivrToSti, // ST(i) = ST(0) / ST(i)
    // push a constant
    Fld1,
    Fldz,
    // register/stack arithmetic (use `sti`)
    FaddP,   // ST(i) += ST(0); pop
    FsubP,   // ST(i) -= ST(0); pop
    FsubrP,  // ST(i) = ST(0) - ST(i); pop
    FmulP,   // ST(i) *= ST(0); pop
    FdivP,   // ST(i) /= ST(0); pop
    FdivrP,  // ST(i) = ST(0) / ST(i); pop
    FaddSti, // ST(0) += ST(i)
    FmulSti, // ST(0) *= ST(i)
    Fxch,    // swap ST(0), ST(i)
    // compare ST(0) with ST(i), set EFLAGS (ZF/PF/CF); the `P` forms pop
    Fucomi,
    Fucomip,
    Fcomi,
    Fcomip,
    // unary on ST(0)
    Fabs,
    Fchs,
    // control word / status word
    Fldcw,  // load control word from memory
    Fnstcw, // store control word to memory
    Fnstsw, // store status word to AX
    Fprem,  // ST(0) = ST(0) rem ST(1)
}

// gpr[] slot for RAX (fnstsw ax).
const RAX: usize = 0;

fn push(cpu: &mut CpuState, bits: u64) {
    cpu.fpu_top = (cpu.fpu_top.wrapping_sub(1)) & 7;
    cpu.fpr[cpu.fpu_top as usize] = bits;
}

fn pop(cpu: &mut CpuState) -> u64 {
    let v = cpu.fpr[cpu.fpu_top as usize];
    cpu.fpu_top = (cpu.fpu_top + 1) & 7;
    v
}

fn st(cpu: &CpuState, i: u8) -> u64 {
    cpu.fpr[((cpu.fpu_top + i as u32) & 7) as usize]
}

fn set_st(cpu: &mut CpuState, i: u8, bits: u64) {
    cpu.fpr[((cpu.fpu_top + i as u32) & 7) as usize] = bits;
}

fn f(bits: u64) -> f64 {
    f64::from_bits(bits)
}

/// Decode an 80-bit extended value to the nearest `f64` (the extra mantissa bits
/// are dropped). Handles zero, normals, and inf/NaN; subnormals collapse to their
/// `f64` rounding.
pub fn f80_to_f64(b: &[u8; 10]) -> u64 {
    let mantissa = u64::from_le_bytes(b[0..8].try_into().unwrap());
    let se = u16::from_le_bytes([b[8], b[9]]);
    let sign = (se >> 15) as u64;
    let exp80 = (se & 0x7fff) as i32;
    if exp80 == 0 && mantissa == 0 {
        return sign << 63; // signed zero
    }
    if exp80 == 0x7fff {
        // inf / NaN
        let frac = (mantissa << 1) >> 12; // top 52 bits below the integer bit
        let nan = if mantissa & !(1u64 << 63) != 0 {
            frac.max(1)
        } else {
            0
        };
        return (sign << 63) | (0x7ffu64 << 52) | nan;
    }
    // normal: bias 16383 -> 1023; mantissa top bit is the explicit integer bit.
    let exp = exp80 - 16383 + 1023;
    if exp <= 0 {
        return sign << 63; // underflow to zero (approximation)
    }
    if exp >= 0x7ff {
        return (sign << 63) | (0x7ffu64 << 52); // overflow to inf
    }
    let frac = (mantissa << 1) >> 12; // drop integer bit, keep 52 bits
    (sign << 63) | ((exp as u64) << 52) | frac
}

/// Encode an `f64` as an 80-bit extended value (exact — every `f64` fits).
pub fn f64_to_f80(bits: u64) -> [u8; 10] {
    let sign = (bits >> 63) & 1;
    let exp64 = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & 0xf_ffff_ffff_ffff;
    let (exp80, mantissa): (u16, u64) = if exp64 == 0 && frac == 0 {
        (0, 0) // signed zero
    } else if exp64 == 0x7ff {
        (0x7fff, (1u64 << 63) | (frac << 11)) // inf / NaN
    } else if exp64 == 0 {
        // subnormal f64: normalize into the 80-bit range.
        let shift = frac.leading_zeros() - 11;
        let m = (frac << (shift + 1)) & 0xf_ffff_ffff_ffff;
        let e = (1 - 1023 - shift as i32) + 16383;
        (e as u16, (1u64 << 63) | (m << 11))
    } else {
        let e = (exp64 - 1023 + 16383) as u16;
        (e, (1u64 << 63) | (frac << 11))
    };
    let mut out = [0u8; 10];
    out[0..8].copy_from_slice(&mantissa.to_le_bytes());
    let se = ((sign as u16) << 15) | (exp80 & 0x7fff);
    out[8..10].copy_from_slice(&se.to_le_bytes());
    out
}

// --- raw guest memory access (bounds-checked; matches the string helper) ---

/// fxsave/fxrstor (§14): save or restore the 512-byte legacy FP/SSE area at `addr`.
/// Returns `Some((fault_addr, is_write))` on a bounds fault, `None` on success.
///
/// Fidelity: XMM0-15 (offset 160) and FCW (offset 0) round-trip exactly. MXCSR
/// (offset 24) is written as the default `0x1f80` and ignored on restore (rounding
/// is not modeled, §M8-T4). x87 ST0-7 (offset 32, 80-bit slots) use the f64-backed
/// converters — enough for the glibc dynamic loader, which fxsaves to preserve XMM
/// across `_dl_runtime_resolve` and never touches x87.
///
/// # Safety
/// As [`exec_x87`]: `base`/`size` describe the live guest buffer.
pub unsafe fn exec_fxstate(
    cpu: &mut CpuState,
    base: *mut u8,
    mem_size: u64,
    addr: u64,
    restore: bool,
) -> Option<(u64, bool)> {
    // Bounds-check the whole 512-byte region up front.
    if addr.checked_add(512).map(|e| e > mem_size).unwrap_or(true) {
        return Some((addr, !restore));
    }
    let p = base.add(addr as usize);
    if restore {
        let mut buf = [0u8; 512];
        std::ptr::copy_nonoverlapping(p, buf.as_mut_ptr(), 512);
        cpu.fpu_cw = u16::from_le_bytes([buf[0], buf[1]]);
        for i in 0..8 {
            let off = 32 + i * 16;
            let f80: [u8; 10] = buf[off..off + 10].try_into().unwrap();
            cpu.fpr[i] = f80_to_f64(&f80);
        }
        for i in 0..16 {
            let off = 160 + i * 16;
            cpu.xmm[i] = u128::from_le_bytes(buf[off..off + 16].try_into().unwrap());
        }
    } else {
        let mut buf = [0u8; 512];
        buf[0..2].copy_from_slice(&cpu.fpu_cw.to_le_bytes());
        buf[2..4].copy_from_slice(&(((cpu.fpu_top as u16) & 7) << 11).to_le_bytes()); // FSW: TOP
        buf[4] = 0xff; // FTW abridged: all tags valid (simplification)
        buf[24..28].copy_from_slice(&0x1f80u32.to_le_bytes()); // MXCSR default
        buf[28..32].copy_from_slice(&0xffffu32.to_le_bytes()); // MXCSR_MASK
        for i in 0..8 {
            let off = 32 + i * 16;
            buf[off..off + 10].copy_from_slice(&f64_to_f80(cpu.fpr[i]));
        }
        for i in 0..16 {
            let off = 160 + i * 16;
            buf[off..off + 16].copy_from_slice(&cpu.xmm[i].to_le_bytes());
        }
        std::ptr::copy_nonoverlapping(buf.as_ptr(), p, 512);
    }
    None
}

unsafe fn read_n(base: *const u8, size: u64, addr: u64, n: usize) -> Option<[u8; 10]> {
    if addr.checked_add(n as u64)? > size {
        return None;
    }
    let mut buf = [0u8; 10];
    std::ptr::copy_nonoverlapping(base.add(addr as usize), buf.as_mut_ptr(), n);
    Some(buf)
}

unsafe fn write_n(base: *mut u8, size: u64, addr: u64, bytes: &[u8]) -> bool {
    if addr
        .checked_add(bytes.len() as u64)
        .map(|e| e > size)
        .unwrap_or(true)
    {
        return false;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(addr as usize), bytes.len());
    true
}

/// x87 float compare → `(ZF, PF, CF)` (unordered sets all three), matching the
/// `ucomisd` mapping used for SSE compares.
fn fcompare(a: f64, b: f64) -> (bool, bool, bool) {
    match a.partial_cmp(&b) {
        None => (true, true, true),
        Some(std::cmp::Ordering::Equal) => (true, false, false),
        Some(std::cmp::Ordering::Less) => (false, false, true),
        Some(std::cmp::Ordering::Greater) => (false, false, false),
    }
}

/// Execute one x87 op. `base`/`mem_size` bound raw guest memory; `addr` is the
/// (already computed) effective address for memory forms; `sti` selects `ST(i)`
/// for register forms. Returns `Some((addr, is_write))` on a memory fault.
///
/// # Safety
/// `base` points to `mem_size` valid guest bytes for the call.
pub unsafe fn exec_x87(
    cpu: &mut CpuState,
    base: *mut u8,
    mem_size: u64,
    kind: FpuKind,
    addr: u64,
    sti: u8,
) -> Option<(u64, bool)> {
    use FpuKind::*;
    match kind {
        FldF64 => push(
            cpu,
            u64::from_le_bytes(read_n(base, mem_size, addr, 8)?[0..8].try_into().unwrap()),
        ),
        FldF32 => {
            let b = read_n(base, mem_size, addr, 4)?;
            let v = f32::from_le_bytes(b[0..4].try_into().unwrap()) as f64;
            push(cpu, v.to_bits());
        }
        FldF80 => {
            let b = read_n(base, mem_size, addr, 10)?;
            push(cpu, f80_to_f64(&b));
        }
        FildI16 => {
            let b = read_n(base, mem_size, addr, 2)?;
            let v = i16::from_le_bytes(b[0..2].try_into().unwrap()) as f64;
            push(cpu, v.to_bits());
        }
        FildI32 => {
            let b = read_n(base, mem_size, addr, 4)?;
            let v = i32::from_le_bytes(b[0..4].try_into().unwrap()) as f64;
            push(cpu, v.to_bits());
        }
        FildI64 => {
            let b = read_n(base, mem_size, addr, 8)?;
            let v = i64::from_le_bytes(b[0..8].try_into().unwrap()) as f64;
            push(cpu, v.to_bits());
        }
        FstpF64 | FstF64 => {
            let v = st(cpu, 0);
            if !write_n(base, mem_size, addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            if kind == FstpF64 {
                pop(cpu);
            }
        }
        FstpF32 | FstF32 => {
            let v = f(st(cpu, 0)) as f32;
            if !write_n(base, mem_size, addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            if kind == FstpF32 {
                pop(cpu);
            }
        }
        FstpF80 => {
            let bytes = f64_to_f80(st(cpu, 0));
            if !write_n(base, mem_size, addr, &bytes) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FistpI16 => {
            let v = f(st(cpu, 0)).round_ties_even_x87() as i16;
            if !write_n(base, mem_size, addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FistpI32 => {
            let v = f(st(cpu, 0)).round_ties_even_x87() as i32;
            if !write_n(base, mem_size, addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FistpI64 => {
            let v = f(st(cpu, 0)).round_ties_even_x87() as i64;
            if !write_n(base, mem_size, addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FaddMemF64 | FsubMemF64 | FsubrMemF64 | FmulMemF64 | FdivMemF64 | FdivrMemF64 => {
            let m = f(u64::from_le_bytes(
                read_n(base, mem_size, addr, 8)?[0..8].try_into().unwrap(),
            ));
            let a = f(st(cpu, 0));
            let r = match kind {
                FaddMemF64 => a + m,
                FsubMemF64 => a - m,
                FsubrMemF64 => m - a,
                FmulMemF64 => a * m,
                FdivMemF64 => a / m,
                _ => m / a,
            };
            set_st(cpu, 0, r.to_bits());
        }
        FaddMemF32 | FsubMemF32 | FsubrMemF32 | FmulMemF32 | FdivMemF32 | FdivrMemF32 => {
            let b = read_n(base, mem_size, addr, 4)?;
            let m = f32::from_le_bytes(b[0..4].try_into().unwrap()) as f64;
            let a = f(st(cpu, 0));
            let r = match kind {
                FaddMemF32 => a + m,
                FsubMemF32 => a - m,
                FsubrMemF32 => m - a,
                FmulMemF32 => a * m,
                FdivMemF32 => a / m,
                _ => m / a,
            };
            set_st(cpu, 0, r.to_bits());
        }
        FldSti => {
            let v = st(cpu, sti);
            push(cpu, v);
        }
        Fld1 => push(cpu, 1.0f64.to_bits()),
        Fldz => push(cpu, 0.0f64.to_bits()),
        FaddP | FsubP | FsubrP | FmulP | FdivP | FdivrP => {
            let s0 = f(st(cpu, 0));
            let si = f(st(cpu, sti));
            let r = match kind {
                FaddP => si + s0,
                FsubP => si - s0,
                FsubrP => s0 - si,
                FmulP => si * s0,
                FdivP => si / s0,
                _ => s0 / si,
            };
            set_st(cpu, sti, r.to_bits());
            pop(cpu);
        }
        FstSti | FstpSti => {
            // fst/fstp st(i): copy ST(0) into ST(i); the `p` form then pops.
            let v = st(cpu, 0);
            set_st(cpu, sti, v);
            if kind == FstpSti {
                pop(cpu);
            }
        }
        FaddSti | FsubSti | FsubrSti | FmulSti | FdivSti | FdivrSti => {
            // Register-form arithmetic with ST(0) as the destination (no pop).
            let s0 = f(st(cpu, 0));
            let si = f(st(cpu, sti));
            let r = match kind {
                FaddSti => s0 + si,
                FsubSti => s0 - si,
                FsubrSti => si - s0,
                FmulSti => s0 * si,
                FdivSti => s0 / si,
                _ => si / s0,
            };
            set_st(cpu, 0, r.to_bits());
        }
        FaddToSti | FsubToSti | FsubrToSti | FmulToSti | FdivToSti | FdivrToSti => {
            // Register-form arithmetic with ST(i) as the destination (no pop).
            let s0 = f(st(cpu, 0));
            let si = f(st(cpu, sti));
            let r = match kind {
                FaddToSti => si + s0,
                FsubToSti => si - s0,
                FsubrToSti => s0 - si,
                FmulToSti => si * s0,
                FdivToSti => si / s0,
                _ => s0 / si,
            };
            set_st(cpu, sti, r.to_bits());
        }
        Fxch => {
            let a = st(cpu, 0);
            let b = st(cpu, sti);
            set_st(cpu, 0, b);
            set_st(cpu, sti, a);
        }
        Fucomi | Fucomip | Fcomi | Fcomip => {
            let (zf, pf, cf) = fcompare(f(st(cpu, 0)), f(st(cpu, sti)));
            cpu.flags.zf = zf;
            cpu.flags.pf = pf;
            cpu.flags.cf = cf;
            cpu.flags.of = false;
            cpu.flags.sf = false;
            cpu.flags.af = false;
            if matches!(kind, Fucomip | Fcomip) {
                pop(cpu);
            }
        }
        Fabs => set_st(cpu, 0, f(st(cpu, 0)).abs().to_bits()),
        Fchs => set_st(cpu, 0, (-f(st(cpu, 0))).to_bits()),
        Fldcw => {
            let b = read_n(base, mem_size, addr, 2)?;
            cpu.fpu_cw = u16::from_le_bytes([b[0], b[1]]);
        }
        Fnstcw => {
            if !write_n(base, mem_size, addr, &cpu.fpu_cw.to_le_bytes()) {
                return Some((addr, true));
            }
        }
        Fnstsw => {
            // Status word: TOP in bits 11–13; condition codes left at 0 (the -i
            // compares set EFLAGS directly, so guests rarely read C0–C3 here).
            let sw = (cpu.fpu_top as u16 & 7) << 11;
            cpu.write_gpr(RAX, sw as u64, 2);
        }
        Fprem => {
            let a = f(st(cpu, 0));
            let b = f(st(cpu, 1));
            set_st(cpu, 0, (a % b).to_bits());
        }
    }
    None
}

/// x87 rounds to nearest even by default; `f64` has no such method at our MSRV.
trait RoundTiesEven {
    fn round_ties_even_x87(self) -> f64;
}
impl RoundTiesEven for f64 {
    fn round_ties_even_x87(self) -> f64 {
        let floor = self.floor();
        let diff = self - floor;
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
}
