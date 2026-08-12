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
    // control), then pop — glibc number formatting uses it (task-139).
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
    // ST(0) op= integer memory (task-233): `DA /n` = m32int, `DE /n` = m16int. Any
    // i16/i32 converts exactly to F80, so the only rounding is the arithmetic itself,
    // taken on the same F80 path as the float-memory forms above. The `r` forms
    // reverse the operands — `fisub` is ST(0) - mem, `fisubr` is mem - ST(0), and
    // likewise `fidiv`/`fidivr` (measured on hardware, SDM Vol 2 FISUB/FIDIV).
    //
    // `ficom`/`ficomp` (`DA /2 /3`, `DE /2 /3`) are deliberately NOT lifted. They
    // report their result in the status-word condition codes C0/C2/C3, which this FPU
    // does not model at all — the status word carries only TOP (see [`env28`]). The
    // `Fcomi`/`Fucomi` family works only because it writes EFLAGS instead.
    FiaddMemI16,
    FiaddMemI32,
    FisubMemI16,
    FisubMemI32,
    FisubrMemI16,
    FisubrMemI32,
    FimulMemI16,
    FimulMemI32,
    FidivMemI16,
    FidivMemI32,
    FidivrMemI16,
    FidivrMemI32,
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
    Fldcw,     // load control word from memory
    Fnstcw,    // store control word to memory
    Fnstsw,    // store status word to AX (register form)
    FnstswMem, // store status word to [mem] (memory form)
    /// `fnstenv m28byte`: store the 28-byte x87 environment, then mask every FP
    /// exception. Only the 28-byte (32-bit-layout) image is lifted — see
    /// [`env28`] for the field-by-field fidelity note.
    Fnstenv,
    /// `fldenv m28byte`: load the 28-byte x87 environment — the restore half of the
    /// pair FreeBSD's `<fenv.h>` wraps around `powf`/`expf`. Environment only: the
    /// eight data registers are untouched (that is `frstor`). See [`load_env28`] for
    /// the field-by-field decision.
    Fldenv,
    Fprem, // ST(0) = ST(0) rem ST(1)
    // Transcendentals (task-150). f64-precision (see `F80` transcendental methods);
    // validated to a bounded ULP vs libm/Unicorn. The reduction-domain ops (fsin/fcos/
    // fptan/fsincos) leave the operand unchanged when |ST(0)| >= 2^63 (hardware sets C2,
    // which is not modeled — the -i compares set EFLAGS, so guests rarely read C0-C3).
    Fsin,    // ST(0) = sin(ST(0))
    Fcos,    // ST(0) = cos(ST(0))
    Fptan,   // ST(0) = tan(ST(0)); then push 1.0
    Fpatan,  // ST(1) = atan2(ST(1), ST(0)); pop
    F2xm1,   // ST(0) = 2^ST(0) - 1
    Fyl2x,   // ST(1) = ST(1) * log2(ST(0)); pop
    Fyl2xp1, // ST(1) = ST(1) * log2(ST(0) + 1); pop
    Fsincos, // ST(0) = sin(ST(0)); then push cos(ST(0))
    // x87 unit management. `fninit` reinitializes the FPU (control word 0x037F,
    // status word 0, tag word all-empty, TOP 0); `fnclex` clears the exception
    // flags. The waiting forms (`finit`/`fclex`) map to the same kinds — FP
    // exceptions and the wait are not modeled, so the wait is a no-op.
    Fninit, // reset control word / status word / TOP
    Fnclex, // clear the exception flags
}

/// x87 argument-reduction domain for `fsin`/`fcos`/`fptan`/`fsincos`: hardware reduces
/// only when the operand is finite with `|ST(0)| < 2^63` (`exp < 63`); outside it, C2 is
/// set and the operand is left unchanged. Inf/NaN flow through the compute path (→ NaN).
fn in_reduction_domain(v: F80) -> bool {
    use crate::f80::Class;
    match v.class {
        Class::Zero | Class::Nan | Class::Inf | Class::Unsupported => true,
        Class::Normal => v.exp < 63,
    }
}

// gpr[] slot for RAX (fnstsw ax).
const RAX: usize = 0;

// `fpr[]` holds raw 80-bit bytes (task-152); x87 arithmetic decodes to/from `F80` at the
// stack boundary here. The round-trip is exact for the normal floats x87 produces.
fn push(cpu: &mut CpuState, v: F80) {
    push_raw(cpu, v.to_bytes());
}

/// Push ten raw bytes without going through [`F80`] — for `fld m80`, which is a move.
fn push_raw(cpu: &mut CpuState, bytes: [u8; 10]) {
    cpu.fpu_top = (cpu.fpu_top.wrapping_sub(1)) & 7;
    cpu.fpr[cpu.fpu_top as usize] = bytes;
}

fn pop(cpu: &mut CpuState) -> F80 {
    let v = F80::from_bytes(&cpu.fpr[cpu.fpu_top as usize]);
    cpu.fpu_top = (cpu.fpu_top + 1) & 7;
    v
}

fn st(cpu: &CpuState, i: u8) -> F80 {
    F80::from_bytes(&cpu.fpr[((cpu.fpu_top + i as u32) & 7) as usize])
}

fn set_st(cpu: &mut CpuState, i: u8, v: F80) {
    cpu.fpr[((cpu.fpu_top + i as u32) & 7) as usize] = v.to_bytes();
}

// --- raw guest memory access (bounds-checked; matches the string helper) ---

/// fxsave/fxrstor (§14): save or restore the 512-byte legacy FP/SSE area at `addr`.
/// Returns `Some((fault_addr, is_write))` on a bounds fault, `None` on success.
///
/// Fidelity: XMM0-15 (offset 160) and FCW (offset 0) round-trip exactly. MXCSR
/// (offset 24) is written as the default `0x1f80` and ignored on restore (rounding
/// is not modeled, §M8-T4). x87 ST0-7 (offset 32, 80-bit slots) copy the raw `fpr[]`
/// bytes verbatim (task-152) — exact for the glibc dynamic loader, which fxsaves to
/// preserve XMM across `_dl_runtime_resolve` and never touches x87.
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
            cpu.fpr[i] = buf[off..off + 10].try_into().unwrap();
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
            buf[off..off + 10].copy_from_slice(&cpu.fpr[i]);
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

/// Read `n` bytes of a memory operand. `None` means the load FAULTED.
///
/// Do not propagate that `None` out of [`exec_x87`] with `?`: this function's `None` and
/// `exec_x87`'s `None` mean opposite things — here it is a fault, there it is success — so
/// `?` turns an unmapped operand into a silent no-op that advances RIP. Translate it to
/// `Some((addr, false))` at the call site, the way the store paths return
/// `Some((addr, true))`.
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
/// Rounding control: control-word bits 11:10 (SDM Vol 1 §4.8.4, Table 4-8) — `00` nearest
/// (ties to even), `01` toward −∞, `10` toward +∞, `11` toward zero. Witnessed end to end
/// by `x87_fldenv_restores_control_word_matches_unicorn`, which `fistp`s the pair
/// `(0.75, -0.75)` under each mode: the pair separates all four, so a mis-decoded field
/// cannot pass.
fn rc(cpu: &CpuState) -> u8 {
    ((cpu.fpu_cw >> 10) & 0b11) as u8
}

/// **Known divergence, measured on hardware.** Because `11` is never produced, the tag word
/// is wrong for every *empty* slot. Concretely, on this host:
///
/// | sequence | hardware | this engine |
/// |---|---|---|
/// | `fninit; fnstenv` | `0xffff` (all empty) | `0x5555` (all "zero") |
/// | `fninit; fld1; fnstenv` | `0x3fff` | `0x1555` |
///
/// A guest that reads the tag word to find out how many slots are occupied — which is what
/// the field is for — gets "all eight hold zero" instead of "all eight are empty". Pinned by
/// `x87_tag_word_after_fninit_diverges_from_hardware` so the value cannot drift silently, and
/// tracked for a real fix; the fix is architectural stack-emptiness state, not a patch here.
///
/// The x87 tag word (SDM Vol 1 §8.1.7): two bits per **physical** register `R(i)` at
/// bits `2i+1:2i` — `00` valid, `01` zero, `10` special (denormal, unnormal, infinity
/// or NaN), `11` empty. Derived from the live `fpr[]` bytes, which reproduces the three
/// non-empty encodings exactly (verified against hardware). `11` is never produced:
/// this FPU has no stack-emptiness bit — every `fpr[]` slot always holds a value — the
/// same simplification [`exec_fxstate`] makes when it writes an all-valid abridged FTW.
fn tag_word(cpu: &CpuState) -> u16 {
    let mut tw = 0u16;
    for (i, r) in cpu.fpr.iter().enumerate() {
        let exp = u16::from_le_bytes([r[8], r[9]]) & 0x7fff;
        let mant = u64::from_le_bytes(r[0..8].try_into().unwrap());
        let tag: u16 = if exp == 0 && mant == 0 {
            1
        } else if exp == 0 || exp == 0x7fff || mant & (1 << 63) == 0 {
            2
        } else {
            0
        };
        tw |= tag << (2 * i);
    }
    tw
}

/// The 28-byte x87 environment image `fnstenv m28byte` writes (SDM Vol 1 Figure 8-9,
/// the 32-bit protected-mode layout — the one 64-bit mode uses at the default operand
/// size). Each architectural 16-bit word occupies the low half of a dword whose
/// reserved upper half hardware fills with `0xFFFF`, not zero.
///
/// Fidelity, field by field:
///
/// * `0` control word — exact (`fpu_cw`).
/// * `4` status word — TOP only. The C0–C3 condition codes and the exception flags are
///   not modeled anywhere in this FPU and read 0, exactly as `fnstsw` already reports.
/// * `8` tag word — derived, see [`tag_word`].
/// * `12` FPU instruction-pointer offset, `16` CS selector + last opcode, `20` FPU data
///   -pointer offset, `24` data selector — **not modeled**. This FPU tracks no
///   last-instruction pointer, selector or opcode, so the image carries their reset
///   (post-`fninit`) value; that is the whole of what an unmodeled field can say here,
///   since one store cannot partially refuse. [`load_env28`] ignores them symmetrically,
///   so the round trip a `fenv` save/restore performs loses nothing observable.
fn env28(cpu: &CpuState) -> [u8; 28] {
    let mut buf = [0u8; 28];
    buf[0..2].copy_from_slice(&cpu.fpu_cw.to_le_bytes());
    buf[4..6].copy_from_slice(&((cpu.fpu_top as u16 & 7) << 11).to_le_bytes());
    buf[8..10].copy_from_slice(&tag_word(cpu).to_le_bytes());
    for off in [2, 6, 10, 26] {
        buf[off..off + 2].copy_from_slice(&0xffffu16.to_le_bytes());
    }
    buf
}

/// Apply the 28-byte environment image `fldenv m28byte` reads — the inverse of
/// [`env28`], and deliberately narrower than it.
///
/// `fldenv` loads the *environment only*: the eight data registers keep their contents
/// (restoring those is `frstor`), and unlike `fnclex` it clears nothing — whatever the
/// image holds is what lands.
///
/// Field by field, and what this FPU can act on:
///
/// * `0` control word — **honored in full**, the same assignment `fldcw` makes. It
///   carries the rounding control [`rc`] reads for `fist`/`fistp`, so discarding it
///   would be wrong arithmetic with no trap; FreeBSD's `fenv` restore around
///   `powf`/`expf` exists precisely to put this word back.
/// * `4` status word — **TOP only** (`fpu_top`), the one field [`env28`] produces.
///   C0–C3 and the exception flags are modeled nowhere in this FPU, so they are
///   dropped rather than parked where nothing reads them.
/// * `8` tag word — **ignored**. Tags are derived from the live `fpr[]` bytes at every
///   store ([`tag_word`], and the abridged FTW in [`exec_fxstate`]), so there is no tag
///   state to load into, and `fldenv` may not touch `fpr[]` to manufacture one. The
///   single tag a loaded word could carry that derivation cannot — `11`/empty — is the
///   stack-emptiness bit this FPU does not model at all.
/// * `12` FIP, `16` CS selector + last opcode, `20` FDP, `24` data selector —
///   **ignored**, symmetrically with [`env28`] never having produced a real one.
///
/// A loaded control word can **unmask** an exception whose flag is set in the loaded
/// status word, which on hardware raises it on the next FP instruction. This FPU raises
/// no FP exception and tracks no exception flag, so the mask bits are merely stored
/// verbatim (a later `fnstcw`/`fnstenv` reads them back) and nothing is ever raised —
/// the same benign fiction the unmodeled MXCSR already is on the SSE half of the very
/// `fenv_t` this serves (task-82: `ldmxcsr` is a no-op, `stmxcsr` a constant `0x1F80`).
///
/// `fnsave`/`frstor` stay unlifted. They move the eight data registers, and their image
/// holds those in *stack* order `ST(0)..ST(7)` while `fpr[]` is indexed physically and
/// the only other register-image path here ([`exec_fxstate`]) copies it physically.
/// Choosing a convention there is a register-file question with its own differential
/// surface, not a consequence of the environment decision above, so they keep trapping.
fn load_env28(cpu: &mut CpuState, buf: &[u8; 28]) {
    cpu.fpu_cw = u16::from_le_bytes([buf[0], buf[1]]);
    cpu.fpu_top = ((u16::from_le_bytes([buf[4], buf[5]]) >> 11) & 7) as u32;
}

/// ST(0)-destination arithmetic against a memory operand `m` (already widened to
/// F80). The `r` variants reverse the operands.
fn mem_arith(kind: FpuKind, a: F80, m: F80) -> F80 {
    use FpuKind::*;
    match kind {
        FaddMemF64 | FaddMemF32 | FiaddMemI16 | FiaddMemI32 => F80::add(a, m),
        FsubMemF64 | FsubMemF32 | FisubMemI16 | FisubMemI32 => F80::sub(a, m),
        FsubrMemF64 | FsubrMemF32 | FisubrMemI16 | FisubrMemI32 => F80::sub(m, a),
        FmulMemF64 | FmulMemF32 | FimulMemI16 | FimulMemI32 => F80::mul(a, m),
        // A zero divisor raises ZE and yields a correctly-signed infinity (SDM) rather
        // than faulting — that is `F80::div`'s `(_, Zero) => inf(a.sign ^ b.sign)` arm,
        // shared with the float-memory forms. The status flags are not modeled.
        FdivMemF64 | FdivMemF32 | FidivMemI16 | FidivMemI32 => F80::div(a, m),
        _ => F80::div(m, a), // FdivrMem* / FidivrMemI*
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
    // Transcendental precision (task-156): Extended = full-80-bit F80 series, else f64.
    let ext = cpu.x87_precision == crate::state::X87Precision::Extended;
    match kind {
        FldF64 => {
            let Some(b) = read_n(mem, addr, 8) else {
                return Some((addr, false));
            };
            push(
                cpu,
                F80::from_f64(u64::from_le_bytes(b[0..8].try_into().unwrap())),
            );
        }
        FldF32 => {
            let Some(b) = read_n(mem, addr, 4) else {
                return Some((addr, false));
            };
            let v = f32::from_le_bytes(b[0..4].try_into().unwrap());
            push(cpu, F80::from_f64((v as f64).to_bits())); // f32 -> f80 is exact
        }
        FldF80 => {
            let Some(b) = read_n(mem, addr, 10) else {
                return Some((addr, false));
            };
            // The ten bytes go into the register VERBATIM — no decode/re-encode. `fld
            // m80` is a move, not a conversion: measured against hardware, an unnormal
            // and a pseudo-denormal both land in the register exactly as they were in
            // memory, and only reach a rule when arithmetic reads them (task-324). A
            // round trip through `F80` renormalized the pseudo-denormal's exponent from 0
            // to 1 and had nothing to say about the rest.
            push_raw(cpu, b);
        }
        FildI16 => {
            let Some(b) = read_n(mem, addr, 2) else {
                return Some((addr, false));
            };
            push(
                cpu,
                F80::from_i64(i16::from_le_bytes(b[0..2].try_into().unwrap()) as i64),
            );
        }
        FildI32 => {
            let Some(b) = read_n(mem, addr, 4) else {
                return Some((addr, false));
            };
            push(
                cpu,
                F80::from_i64(i32::from_le_bytes(b[0..4].try_into().unwrap()) as i64),
            );
        }
        FildI64 => {
            let Some(b) = read_n(mem, addr, 8) else {
                return Some((addr, false));
            };
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
            // The store side of the same move: `fstp m80` writes the register's bytes.
            let bytes = cpu.fpr[cpu.fpu_top as usize];
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
            let Some(b) = read_n(mem, addr, 8) else {
                return Some((addr, false));
            };
            let m = F80::from_f64(u64::from_le_bytes(b[0..8].try_into().unwrap()));
            let a = st(cpu, 0);
            set_st(cpu, 0, mem_arith(kind, a, m));
        }
        FaddMemF32 | FsubMemF32 | FsubrMemF32 | FmulMemF32 | FdivMemF32 | FdivrMemF32 => {
            let Some(b) = read_n(mem, addr, 4) else {
                return Some((addr, false));
            };
            let v = f32::from_le_bytes(b[0..4].try_into().unwrap());
            let m = F80::from_f64((v as f64).to_bits());
            let a = st(cpu, 0);
            set_st(cpu, 0, mem_arith(kind, a, m));
        }
        // Integer-memory arithmetic (task-233). The operand is read at its architectural
        // width, sign-extended, and widened to F80 — exactly what `FildI16`/`FildI32` do,
        // and exact for every i16/i32 — so the only rounding happens inside `mem_arith`.
        FiaddMemI16 | FisubMemI16 | FisubrMemI16 | FimulMemI16 | FidivMemI16 | FidivrMemI16 => {
            let Some(b) = read_n(mem, addr, 2) else {
                return Some((addr, false));
            };
            let m = F80::from_i64(i16::from_le_bytes(b[0..2].try_into().unwrap()) as i64);
            let a = st(cpu, 0);
            set_st(cpu, 0, mem_arith(kind, a, m));
        }
        FiaddMemI32 | FisubMemI32 | FisubrMemI32 | FimulMemI32 | FidivMemI32 | FidivrMemI32 => {
            let Some(b) = read_n(mem, addr, 4) else {
                return Some((addr, false));
            };
            let m = F80::from_i64(i32::from_le_bytes(b[0..4].try_into().unwrap()) as i64);
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
            cpu.flags.set_pf(pf);
            cpu.flags.cf = cf;
            cpu.flags.of = false;
            cpu.flags.sf = false;
            cpu.flags.set_af(false);
            if matches!(kind, Fucomip | Fcomip) {
                pop(cpu);
            }
        }
        Fabs => set_st(cpu, 0, st(cpu, 0).abs()),
        Fchs => set_st(cpu, 0, st(cpu, 0).neg()),
        Fldcw => {
            let Some(b) = read_n(mem, addr, 2) else {
                return Some((addr, false));
            };
            cpu.fpu_cw = u16::from_le_bytes([b[0], b[1]]);
        }
        Fnstcw => {
            if !mem.store(addr, &cpu.fpu_cw.to_le_bytes()) {
                return Some((addr, true));
            }
        }
        Fnstsw | FnstswMem => {
            // Status word: TOP in bits 11–13; condition codes left at 0 (the -i
            // compares set EFLAGS directly, so guests rarely read C0–C3 here).
            let sw = (cpu.fpu_top as u16 & 7) << 11;
            if kind == FnstswMem {
                // `fnstsw m16`: store the 16-bit status word to memory.
                if !mem.store(addr, &sw.to_le_bytes()) {
                    return Some((addr, true));
                }
            } else {
                // `fnstsw ax`: write it to AX.
                cpu.write_gpr(RAX, sw as u64, 2);
            }
        }
        Fnstenv => {
            if !mem.store(addr, &env28(cpu)) {
                return Some((addr, true));
            }
            // SDM: "…and then masks all floating-point exceptions" — the store is
            // followed by setting all six exception-mask bits in the control word.
            // Confirmed on hardware (CW 0x0360 -> 0x037F across one `fnstenv`).
            cpu.fpu_cw |= 0x3f;
        }
        Fldenv => {
            let mut buf = [0u8; 28];
            if !mem.load(addr, &mut buf) {
                return Some((addr, false));
            }
            load_env28(cpu, &buf);
        }
        Fninit => {
            // Reinitialize the x87 unit: control word 0x037F (round-to-nearest,
            // all exceptions masked, 64-bit precision), status word 0 (which the
            // derived value tracks once TOP is 0), tag word all-empty, TOP 0. The
            // tag word is not stored (fxsave writes an abridged FTW), and exception
            // flags aren't modeled, so resetting the control word and TOP is the
            // full observable effect.
            cpu.fpu_cw = 0x037F;
            cpu.fpu_top = 0;
        }
        Fnclex => {
            // Clear the exception flags. They aren't modeled (the status word only
            // carries TOP and the condition codes read 0), so this is a no-op kept
            // so the opcode lifts instead of faulting.
        }
        Fprem => {
            let (a, b) = (st(cpu, 0), st(cpu, 1));
            set_st(cpu, 0, F80::rem(a, b));
        }
        // --- Transcendentals (task-150; Extended F80 path task-156) ---
        Fsin => {
            let x = st(cpu, 0);
            if in_reduction_domain(x) {
                set_st(cpu, 0, if ext { x.sin_ext() } else { x.sin() });
            }
        }
        Fcos => {
            let x = st(cpu, 0);
            if in_reduction_domain(x) {
                set_st(cpu, 0, if ext { x.cos_ext() } else { x.cos() });
            }
        }
        Fptan => {
            let x = st(cpu, 0);
            if in_reduction_domain(x) {
                set_st(cpu, 0, if ext { x.tan_ext() } else { x.tan() });
                push(cpu, F80::from_i64(1)); // fptan pushes 1.0 (→ ST(1)=tan, ST(0)=1.0)
            }
        }
        Fsincos => {
            let x = st(cpu, 0);
            if in_reduction_domain(x) {
                let (s, c) = if ext {
                    (x.sin_ext(), x.cos_ext())
                } else {
                    (x.sin(), x.cos())
                };
                set_st(cpu, 0, s);
                push(cpu, c); // → ST(0)=cos, ST(1)=sin
            }
        }
        Fpatan => {
            let (a, b) = (st(cpu, 1), st(cpu, 0));
            let r = if ext {
                F80::atan2_ext(a, b)
            } else {
                F80::atan2(a, b)
            }; // atan(ST1/ST0), full quadrant
            set_st(cpu, 1, r);
            pop(cpu);
        }
        F2xm1 => {
            let x = st(cpu, 0);
            set_st(cpu, 0, if ext { x.exp2m1_ext() } else { x.exp2m1() });
        }
        Fyl2x => {
            let (y, x) = (st(cpu, 1), st(cpu, 0));
            let r = if ext {
                F80::ylog2x_ext(y, x)
            } else {
                F80::ylog2x(y, x)
            };
            set_st(cpu, 1, r);
            pop(cpu);
        }
        Fyl2xp1 => {
            let (y, x) = (st(cpu, 1), st(cpu, 0));
            let r = if ext {
                F80::ylog2xp1_ext(y, x)
            } else {
                F80::ylog2xp1(y, x)
            };
            set_st(cpu, 1, r);
            pop(cpu);
        }
    }
    None
}
