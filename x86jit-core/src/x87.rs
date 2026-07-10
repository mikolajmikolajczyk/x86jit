//! x87 FPU (§14), backed by the architectural 80-bit extended precision ([`F80`],
//! `f80.rs`) — each register holds the full sign + 15-bit exponent + 64-bit
//! significand and every op rounds to nearest-even at 64 significand bits, matching
//! real hardware. One `exec_x87` routine drives both backends (the interpreter calls
//! it directly; the JIT via a helper), so they agree bit-for-bit with each other and
//! with Unicorn — including the extra 11 mantissa bits that an `f64`-backed register
//! file would drop (e.g. `printf("%Lf")` long-double formatting).
//!
//! The register file is a stack: `ST(i)` = `fpr[(fpu_top + i) & 7]`. `fld`-style
//! ops decrement `fpu_top` then write `ST(0)`; `fstp`-style ops read `ST(0)` then
//! increment. Memory operands go through [`FpMem`], so the interpreter gets the
//! same region check + SMC `note_write` as a scalar store while the JIT keeps a raw
//! bounds-only view; a fault returns `Some((addr, is_write))` so the caller traps
//! with RIP on the instruction (§8, §16), exactly like the string helper.

use crate::f80::F80;
use crate::state::CpuState;

/// Guest-memory access for the x87 helpers. Two implementors give the two backends
/// the memory semantics each already uses for a scalar store:
///
/// * The interpreter passes `&Memory` — reads/writes go through a mapped-RAM region
///   check and, on a write, the SMC `note_write` (§10), so a self-modifying x87
///   store onto a code page invalidates just like `IrOp::Store`.
/// * The JIT passes [`RawFpMem`] — a bounds-only raw view matching its inlined
///   stores; JIT-side SMC is the deferred "mark host code dead" step (§10, §9.1).
///
/// A `Trap` (MMIO) region faults as unmapped here: an x87 store's value (up to a
/// 10-byte f80 / 512-byte fxsave) can't fit `Exit::MmioWrite`, so x87→MMIO is
/// deferred rather than misreported (§5.2).
pub trait FpMem {
    /// Fill `buf` from guest memory; `false` on a fault (unmapped / non-RAM).
    fn load(&self, addr: u64, buf: &mut [u8]) -> bool;
    /// Write `bytes` to guest memory (recording SMC); `false` on a fault.
    fn store(&self, addr: u64, bytes: &[u8]) -> bool;
}

impl FpMem for crate::memory::Memory {
    fn load(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.read_ram_guest(addr, buf)
    }
    fn store(&self, addr: u64, bytes: &[u8]) -> bool {
        self.write_ram_guest(addr, bytes)
    }
}

/// Bounds-only raw guest view for the JIT x87/fxstate helpers (deferred JIT SMC).
/// `base` is the host address of guest `guest_base`; `size` is the exclusive top guest
/// address. A guest address `a` reads/writes `base + (a - guest_base)`, valid iff
/// `guest_base <= a` and `a + len <= size` (see [`crate::interp::RawStrMem`]).
pub struct RawFpMem {
    pub base: *mut u8,
    pub size: u64,
    pub guest_base: u64,
}

impl RawFpMem {
    /// Backing offset for `addr` if `[addr, addr+len)` lies in `[guest_base, size)`.
    #[inline]
    fn off(&self, addr: u64, len: usize) -> Option<usize> {
        let end = addr.checked_add(len as u64)?;
        if addr < self.guest_base || end > self.size {
            return None;
        }
        Some((addr - self.guest_base) as usize)
    }
}

impl FpMem for RawFpMem {
    fn load(&self, addr: u64, buf: &mut [u8]) -> bool {
        let Some(off) = self.off(addr, buf.len()) else {
            return false;
        };
        // SAFETY: bounds-checked into `[guest_base, size)`; `base` is guest `guest_base`.
        unsafe {
            std::ptr::copy_nonoverlapping(self.base.add(off), buf.as_mut_ptr(), buf.len());
        }
        true
    }
    fn store(&self, addr: u64, bytes: &[u8]) -> bool {
        let Some(off) = self.off(addr, bytes.len()) else {
            return false;
        };
        // SAFETY: bounds-checked into `[guest_base, size)`; `base` is guest `guest_base`.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(off), bytes.len());
        }
        true
    }
}

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
    // fisttp (SSE3): store integer truncating toward zero (ignores the FPU rounding
    // control), then pop — glibc number formatting uses it (task-195).
    FisttpI16,
    FisttpI32,
    FisttpI64,
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

fn push(cpu: &mut CpuState, v: F80) {
    cpu.fpu_top = (cpu.fpu_top.wrapping_sub(1)) & 7;
    cpu.fpr[cpu.fpu_top as usize] = v;
}

fn pop(cpu: &mut CpuState) -> F80 {
    let v = cpu.fpr[cpu.fpu_top as usize];
    cpu.fpu_top = (cpu.fpu_top + 1) & 7;
    v
}

fn st(cpu: &CpuState, i: u8) -> F80 {
    cpu.fpr[((cpu.fpu_top + i as u32) & 7) as usize]
}

fn set_st(cpu: &mut CpuState, i: u8, v: F80) {
    cpu.fpr[((cpu.fpu_top + i as u32) & 7) as usize] = v;
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
pub fn exec_fxstate<M: FpMem>(
    cpu: &mut CpuState,
    mem: &M,
    addr: u64,
    restore: bool,
) -> Option<(u64, bool)> {
    if restore {
        let mut buf = [0u8; 512];
        if !mem.load(addr, &mut buf) {
            return Some((addr, false));
        }
        cpu.fpu_cw = u16::from_le_bytes([buf[0], buf[1]]);
        for i in 0..8 {
            let off = 32 + i * 16;
            let f80: [u8; 10] = buf[off..off + 10].try_into().unwrap();
            cpu.fpr[i] = F80::from_bytes(&f80);
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
            buf[off..off + 10].copy_from_slice(&cpu.fpr[i].to_bytes());
        }
        for i in 0..16 {
            let off = 160 + i * 16;
            buf[off..off + 16].copy_from_slice(&cpu.xmm[i].to_le_bytes());
        }
        if !mem.store(addr, &buf) {
            return Some((addr, true));
        }
    }
    None
}

fn read_n<M: FpMem>(mem: &M, addr: u64, n: usize) -> Option<[u8; 10]> {
    let mut buf = [0u8; 10];
    if mem.load(addr, &mut buf[..n]) {
        Some(buf)
    } else {
        None
    }
}

/// x87 float compare → `(ZF, PF, CF)` (unordered sets all three), matching the
/// `ucomisd` mapping used for SSE compares.
/// The FPU control-word rounding-control field (bits 10-11): 0 nearest, 1 down,
/// 2 up, 3 truncate — the rounding mode for `fist`/`fistp`.
fn rc(cpu: &CpuState) -> u8 {
    ((cpu.fpu_cw >> 10) & 0b11) as u8
}

/// ST(0)-destination arithmetic against a memory operand `m` (already widened to
/// F80). The `r` variants reverse the operands.
fn mem_arith(kind: FpuKind, a: F80, m: F80) -> F80 {
    use FpuKind::*;
    match kind {
        FaddMemF64 | FaddMemF32 => F80::add(a, m),
        FsubMemF64 | FsubMemF32 => F80::sub(a, m),
        FsubrMemF64 | FsubrMemF32 => F80::sub(m, a),
        FmulMemF64 | FmulMemF32 => F80::mul(a, m),
        FdivMemF64 | FdivMemF32 => F80::div(a, m),
        _ => F80::div(m, a), // FdivrMem*
    }
}

/// Execute one x87 op. `mem` is the guest memory (see [`FpMem`]); `addr` is the
/// (already computed) effective address for memory forms; `sti` selects `ST(i)`
/// for register forms. Returns `Some((addr, is_write))` on a memory fault.
pub fn exec_x87<M: FpMem>(
    cpu: &mut CpuState,
    mem: &M,
    kind: FpuKind,
    addr: u64,
    sti: u8,
) -> Option<(u64, bool)> {
    use FpuKind::*;
    match kind {
        FldF64 => {
            let b = read_n(mem, addr, 8)?;
            push(
                cpu,
                F80::from_f64(u64::from_le_bytes(b[0..8].try_into().unwrap())),
            );
        }
        FldF32 => {
            let b = read_n(mem, addr, 4)?;
            let v = f32::from_le_bytes(b[0..4].try_into().unwrap());
            push(cpu, F80::from_f64((v as f64).to_bits())); // f32 -> f80 is exact
        }
        FldF80 => {
            let b = read_n(mem, addr, 10)?;
            push(cpu, F80::from_bytes(&b));
        }
        FildI16 => {
            let b = read_n(mem, addr, 2)?;
            push(
                cpu,
                F80::from_i64(i16::from_le_bytes(b[0..2].try_into().unwrap()) as i64),
            );
        }
        FildI32 => {
            let b = read_n(mem, addr, 4)?;
            push(
                cpu,
                F80::from_i64(i32::from_le_bytes(b[0..4].try_into().unwrap()) as i64),
            );
        }
        FildI64 => {
            let b = read_n(mem, addr, 8)?;
            push(
                cpu,
                F80::from_i64(i64::from_le_bytes(b[0..8].try_into().unwrap())),
            );
        }
        FstpF64 | FstF64 => {
            let v = st(cpu, 0).to_f64();
            if !mem.store(addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            if kind == FstpF64 {
                pop(cpu);
            }
        }
        FstpF32 | FstF32 => {
            let v = f64::from_bits(st(cpu, 0).to_f64()) as f32;
            if !mem.store(addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            if kind == FstpF32 {
                pop(cpu);
            }
        }
        FstpF80 => {
            let bytes = st(cpu, 0).to_bytes();
            if !mem.store(addr, &bytes) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FistpI16 => {
            let v = st(cpu, 0).to_i64_rc(rc(cpu)) as i16;
            if !mem.store(addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FistpI32 => {
            let v = st(cpu, 0).to_i64_rc(rc(cpu)) as i32;
            if !mem.store(addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FistpI64 => {
            let v = st(cpu, 0).to_i64_rc(rc(cpu));
            if !mem.store(addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        // fisttp: like fistp but always truncates toward zero (rc = 3), ignoring the
        // FPU rounding control.
        FisttpI16 => {
            let v = st(cpu, 0).to_i64_rc(3) as i16;
            if !mem.store(addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FisttpI32 => {
            let v = st(cpu, 0).to_i64_rc(3) as i32;
            if !mem.store(addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FisttpI64 => {
            let v = st(cpu, 0).to_i64_rc(3);
            if !mem.store(addr, &v.to_le_bytes()) {
                return Some((addr, true));
            }
            pop(cpu);
        }
        FaddMemF64 | FsubMemF64 | FsubrMemF64 | FmulMemF64 | FdivMemF64 | FdivrMemF64 => {
            let b = read_n(mem, addr, 8)?;
            let m = F80::from_f64(u64::from_le_bytes(b[0..8].try_into().unwrap()));
            let a = st(cpu, 0);
            set_st(cpu, 0, mem_arith(kind, a, m));
        }
        FaddMemF32 | FsubMemF32 | FsubrMemF32 | FmulMemF32 | FdivMemF32 | FdivrMemF32 => {
            let b = read_n(mem, addr, 4)?;
            let v = f32::from_le_bytes(b[0..4].try_into().unwrap());
            let m = F80::from_f64((v as f64).to_bits());
            let a = st(cpu, 0);
            set_st(cpu, 0, mem_arith(kind, a, m));
        }
        FldSti => {
            let v = st(cpu, sti);
            push(cpu, v);
        }
        Fld1 => push(cpu, F80::from_i64(1)),
        Fldz => push(cpu, F80::zero(false)),
        FaddP | FsubP | FsubrP | FmulP | FdivP | FdivrP => {
            let (s0, si) = (st(cpu, 0), st(cpu, sti));
            let r = match kind {
                FaddP => F80::add(si, s0),
                FsubP => F80::sub(si, s0),
                FsubrP => F80::sub(s0, si),
                FmulP => F80::mul(si, s0),
                FdivP => F80::div(si, s0),
                _ => F80::div(s0, si),
            };
            set_st(cpu, sti, r);
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
            let (s0, si) = (st(cpu, 0), st(cpu, sti));
            let r = match kind {
                FaddSti => F80::add(s0, si),
                FsubSti => F80::sub(s0, si),
                FsubrSti => F80::sub(si, s0),
                FmulSti => F80::mul(s0, si),
                FdivSti => F80::div(s0, si),
                _ => F80::div(si, s0),
            };
            set_st(cpu, 0, r);
        }
        FaddToSti | FsubToSti | FsubrToSti | FmulToSti | FdivToSti | FdivrToSti => {
            // Register-form arithmetic with ST(i) as the destination (no pop).
            let (s0, si) = (st(cpu, 0), st(cpu, sti));
            let r = match kind {
                FaddToSti => F80::add(si, s0),
                FsubToSti => F80::sub(si, s0),
                FsubrToSti => F80::sub(s0, si),
                FmulToSti => F80::mul(si, s0),
                FdivToSti => F80::div(si, s0),
                _ => F80::div(s0, si),
            };
            set_st(cpu, sti, r);
        }
        Fxch => {
            let (a, b) = (st(cpu, 0), st(cpu, sti));
            set_st(cpu, 0, b);
            set_st(cpu, sti, a);
        }
        Fucomi | Fucomip | Fcomi | Fcomip => {
            let (zf, pf, cf) = F80::compare(st(cpu, 0), st(cpu, sti));
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
        Fabs => set_st(cpu, 0, st(cpu, 0).abs()),
        Fchs => set_st(cpu, 0, st(cpu, 0).neg()),
        Fldcw => {
            let b = read_n(mem, addr, 2)?;
            cpu.fpu_cw = u16::from_le_bytes([b[0], b[1]]);
        }
        Fnstcw => {
            if !mem.store(addr, &cpu.fpu_cw.to_le_bytes()) {
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
            let (a, b) = (st(cpu, 0), st(cpu, 1));
            set_st(cpu, 0, F80::rem(a, b));
        }
    }
    None
}
