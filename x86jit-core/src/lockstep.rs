//! Env-gated lockstep trace capture for the interpreter (debug/forensics only).
//!
//! When `X86JIT_LOCKSTEP=<path>` is set, [`interpret_block`](crate::interp::interpret_block)
//! records every vector instruction it executes — register-only *and* memory-source —
//! as `(guest_addr, bytes, gprs, optional mem operand, pre/post ymm0-15)`. A native
//! replay harness (`x86jit-tests`) re-runs each op on the real host CPU from the same
//! pre-state and reports the first op whose result diverges from the captured
//! (interpreter) post-state — i.e. the exact op, with openssl's real operands, we
//! compute wrong. This hunts an operand-specific bug in a composed routine (e.g.
//! `rsaz_1024_*_avx2`) that per-op fuzzing can't reach.
//!
//! Zero cost unless the env var is set: [`begin`] returns an inert [`Bracket`] and
//! every hook short-circuits on a disabled sink.
//!
//! Bracketing relies on a structural invariant: a block ends *at* control flow, so a
//! vector instruction is never the last op of a block — its post-state is always the
//! cpu state observed at the *next* `InsnStart`. Consecutive `InsnStart`s therefore
//! bracket one instruction (`post_i == pre_{i+1}`) with no end-of-block flush needed.

use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use iced_x86::{Decoder, DecoderOptions, EncodingKind, Instruction, Mnemonic, OpKind, Register};

use crate::memory::Memory;
use crate::state::CpuState;

/// The 16 architectural vector registers we snapshot (xmm/ymm 0..15). AVX2 crypto —
/// the target — never touches ymm16..31, zmm, or opmask, and the native stub loads
/// exactly this window, so we scope capture to replayable ops only.
const NVEC: usize = 16;
/// Bytes of memory captured around a memory-operand's effective address (ymm = 32B;
/// 64 covers any single AVX2 operand with slack).
const MEM_BYTES: usize = 64;

/// Low 128 bits + upper 128 bits of ymm0..15, captured before/after one instruction.
#[derive(Clone, Copy)]
struct Snap {
    xmm: [u128; NVEC],
    ymm_hi: [u128; NVEC],
}

impl Snap {
    fn of(cpu: &CpuState) -> Self {
        let mut s = Snap {
            xmm: [0; NVEC],
            ymm_hi: [0; NVEC],
        };
        s.xmm.copy_from_slice(&cpu.xmm[..NVEC]);
        s.ymm_hi.copy_from_slice(&cpu.ymm_hi[..NVEC]);
        s
    }
}

/// The full architectural side-state of one vector instruction: gpr, flags, the
/// optional memory-operand window, and the vector snapshot. Captured before and after
/// so the replay can verify *any* effect a vector op has — a register write, a store,
/// a GPR↔vector transfer, or a flag-setting compare (`comisd`/`ptest`).
#[derive(Clone, Copy)]
struct SideState {
    gpr: [u64; 16],
    flags: u64,
    mem: [u8; MEM_BYTES],
    snap: Snap,
}

struct Pending {
    addr: u64,
    bytes: Vec<u8>,
    /// `Some(ea)` for a memory-operand op (source or store); `None` for register-only.
    ea: Option<u64>,
    pre: SideState,
}

/// Per-`interpret_block`-call bracket state. `None` inner = capture disabled (the
/// common case): all hooks are no-ops.
pub struct Bracket {
    inner: Option<Option<Pending>>,
}

impl Bracket {
    #[inline]
    pub fn active(&self) -> bool {
        self.inner.is_some()
    }
}

/// Start a bracket for one block. Cheap: returns an inert `Bracket` when the sink is
/// disabled so the caller's hot loop pays only a branch.
#[inline]
pub fn begin() -> Bracket {
    Bracket {
        inner: if sink().is_some() { Some(None) } else { None },
    }
}

/// Called at each `IrOp::InsnStart`. The cpu now holds the *post*-state of the
/// previous instruction (== *pre*-state of the one at `addr`). Flush the previous
/// capture if any, then arm a fresh one when the upcoming instruction is a replayable
/// vector op.
pub fn on_insn_start(b: &mut Bracket, cpu: &CpuState, mem: &Memory, addr: u64) {
    let Some(pending) = b.inner.as_mut() else {
        return;
    };
    if let Some(p) = pending.take() {
        let post = side_state(cpu, mem, p.ea);
        write_record(&p, &post);
    }
    let Ok(code) = mem.code_slice(addr, 15) else {
        return;
    };
    // When an address window is set (X86JIT_LOCKSTEP_LO/HI), capture only in-window and
    // also cover scalar big-integer arithmetic there — used to hunt a bug on openssl's
    // rsaz-avx2 path, whose scalar carry-chain glue (mul/mulx/adc/…) the vector-only
    // capture can't see. With no window, keep the original vector-only, any-address mode.
    let allow_scalar = match window() {
        Some((lo, hi)) => {
            if addr < lo || addr >= hi {
                return;
            }
            true
        }
        None => false,
    };
    let mut dec = Decoder::with_ip(64, code, addr, DecoderOptions::NONE);
    let mut insn = Instruction::default();
    dec.decode_out(&mut insn);
    let Some(mem_op) = replayable_op(&insn, code, allow_scalar) else {
        return;
    };
    // Resolve the effective address for a memory-operand op (source or store).
    let ea = match mem_op {
        Some(opi) => {
            match insn.virtual_address(opi, 0, |reg, _, _| Some(reg_value(cpu, reg))) {
                Some(ea) => Some(ea),
                None => return, // unresolved EA → can't replay, drop
            }
        }
        None => None,
    };
    *pending = Some(Pending {
        addr,
        bytes: code[..insn.len()].to_vec(),
        ea,
        pre: side_state(cpu, mem, ea),
    });
}

/// Snapshot the gpr/flags/mem/vec side-state around one instruction. `ea` supplies the
/// memory-operand window to capture (zero-filled when absent or unreadable).
fn side_state(cpu: &CpuState, mem: &Memory, ea: Option<u64>) -> SideState {
    let mut gpr = [0u64; 16];
    gpr.copy_from_slice(&cpu.gpr[..16]);
    let mut membuf = [0u8; MEM_BYTES];
    if let Some(ea) = ea {
        for (i, slot) in membuf.iter_mut().enumerate() {
            *slot = mem.read(ea + i as u64, 1).map(|v| v as u8).unwrap_or(0);
        }
    }
    SideState {
        gpr,
        flags: flags_bits(cpu),
        mem: membuf,
        snap: Snap::of(cpu),
    }
}

/// Pack the six arithmetic flags into an rflags-shaped word (positions match x86).
fn flags_bits(cpu: &CpuState) -> u64 {
    let f = &cpu.flags;
    (f.cf as u64)
        | (f.pf as u64) << 2
        | (f.af as u64) << 4
        | (f.zf as u64) << 6
        | (f.sf as u64) << 7
        | (f.of as u64) << 11
}

/// Value of an addressing register (base/index) from the cpu — always read at full
/// 64-bit width, which is what effective-address math uses.
fn reg_value(cpu: &CpuState, reg: Register) -> u64 {
    if reg.is_gpr() {
        cpu.gpr[reg.full_register().number()]
    } else {
        // Segment bases: FS/GS are rejected as nonzero by the native harness, and the
        // rsaz path uses stack/normal addressing, so 0 is correct here.
        0
    }
}

/// Classify the instruction: return `Some(mem_op_index)` for a replayable vector op —
/// `Some(Some(i))` if operand `i` is a memory operand (source or store), `Some(None)`
/// if register-only. `None` when not replayable.
///
/// Replayable = VEX/legacy encoding, at least one xmm/ymm operand (so it's a vector
/// instruction) — OR, when `allow_scalar`, a scalar big-integer arithmetic op (mul,
/// mulx, adc, adcx, adox, …). Every register operand is an xmm/ymm (0..15) or a GPR
/// (transfers like `vmovq`/`vpextrq`/`vpinsrq`), at most one memory operand (source OR
/// store), and no opmask/segment operand. The full architectural effect (regs, flags,
/// mem) is verified downstream, so flag-only compares (`comisd`/`ptest`), stores, and
/// scalar carry-chain ops are all in scope.
fn replayable_op(insn: &Instruction, code: &[u8], allow_scalar: bool) -> Option<Option<u32>> {
    if insn.is_invalid() || insn.len() == 0 || insn.len() > code.len() {
        return None;
    }
    match insn.encoding() {
        EncodingKind::VEX | EncodingKind::Legacy => {}
        _ => return None,
    }
    let mut mem_op = None;
    let mut has_vec = false;
    for i in 0..insn.op_count() {
        match insn.op_kind(i) {
            OpKind::Register => {
                let r = insn.op_register(i);
                if r.is_xmm() || r.is_ymm() {
                    if r.number() >= NVEC {
                        return None; // ymm16..31 / zmm — outside the stub window
                    }
                    has_vec = true;
                } else if !r.is_gpr() {
                    return None; // opmask / segment / other → not replayable
                }
            }
            OpKind::Memory => {
                if mem_op.is_some() {
                    return None; // more than one memory operand — unexpected, skip
                }
                // A base/index we can't read as a plain GPR (rIP is fine) → skip.
                let b = insn.memory_base();
                let x = insn.memory_index();
                let ok_base = b == Register::None || b == Register::RIP || b.is_gpr();
                let ok_index = x == Register::None || x.is_gpr();
                if !ok_base || !ok_index {
                    return None;
                }
                mem_op = Some(i);
            }
            OpKind::Immediate8
            | OpKind::Immediate8_2nd
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate32to64
            | OpKind::Immediate64 => {}
            _ => return None,
        }
    }
    if has_vec || (allow_scalar && replayable_scalar(insn)) {
        Some(mem_op)
    } else {
        None
    }
}

/// v3 record: addr | blen | bytes | has_mem | ea | pre-side | post-side, where each
/// side is gpr[16] | flags | mem[64] | vec-snap(xmm[16]+ymm_hi[16]).
fn write_record(p: &Pending, post: &SideState) {
    let Some(m) = sink() else { return };
    // Optional cap (X86JIT_LOCKSTEP_MAX): stop writing after N records so an all-data-op
    // capture stays bounded to the early rsaz calls, where a systematic op bug shows.
    if let Some(max) = record_cap() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static WRITTEN: AtomicU64 = AtomicU64::new(0);
        if WRITTEN.fetch_add(1, Ordering::Relaxed) >= max {
            return;
        }
    }
    let mut buf = Vec::with_capacity(64 + p.bytes.len() + 2 * side_wire_len());
    buf.extend_from_slice(&p.addr.to_le_bytes());
    buf.push(p.bytes.len() as u8);
    buf.extend_from_slice(&p.bytes);
    buf.push(p.ea.is_some() as u8);
    buf.extend_from_slice(&p.ea.unwrap_or(0).to_le_bytes());
    write_side(&mut buf, &p.pre);
    write_side(&mut buf, post);
    if let Ok(mut f) = m.lock() {
        let _ = f.write_all(&buf);
    }
}

/// Bytes one `SideState` occupies on the wire.
const fn side_wire_len() -> usize {
    16 * 8 + 8 + MEM_BYTES + 2 * NVEC * 16
}

fn write_side(buf: &mut Vec<u8>, s: &SideState) {
    for &g in &s.gpr {
        buf.extend_from_slice(&g.to_le_bytes());
    }
    buf.extend_from_slice(&s.flags.to_le_bytes());
    buf.extend_from_slice(&s.mem);
    for v in s.snap.xmm.iter().chain(s.snap.ymm_hi.iter()) {
        buf.extend_from_slice(&v.to_le_bytes());
    }
}

/// A scalar instruction worth replaying: any non-control-flow op (so we cover data
/// movement — mov/movzx/movsx/cmov/bt/xchg/… — not just arithmetic, since the bug is
/// an untraced op whose wrong output is captured as a correct input downstream), minus
/// nondeterministic / privileged / helper-backed ops that can't be replayed to a clean
/// `hlt` on the host. Operand-kind constraints (gpr/xmm/ymm regs, ≤1 mem, no
/// opmask/segment) are enforced by the caller loop.
fn replayable_scalar(insn: &Instruction) -> bool {
    if insn.flow_control() != iced_x86::FlowControl::Next {
        return false; // branch/call/ret/int/syscall — not a straight-line data op
    }
    !matches!(
        insn.mnemonic(),
        // Nondeterministic / read host state → a native replay would legitimately differ.
        Mnemonic::Cpuid
            | Mnemonic::Rdtsc
            | Mnemonic::Rdtscp
            | Mnemonic::Rdrand
            | Mnemonic::Rdseed
            | Mnemonic::Rdpmc
            | Mnemonic::Rdpid
            | Mnemonic::Xgetbv
            // Flag byte transfers whose result is our elided/materialized flag state, not
            // hardware's — comparing them is the flag-elision noise we already ruled out.
            | Mnemonic::Lahf
            | Mnemonic::Sahf
            | Mnemonic::Pushfq
            | Mnemonic::Popfq
            // Large / privileged state ops (not in the rsaz path; unsafe/huge to replay).
            | Mnemonic::Xsave
            | Mnemonic::Xrstor
            | Mnemonic::Fxsave
            | Mnemonic::Fxrstor
    )
}

/// Optional capture window `[lo, hi)` in guest VA, from `X86JIT_LOCKSTEP_LO`/`_HI`
/// (hex). When set, capture is restricted to it and also covers scalar arithmetic.
fn window() -> Option<(u64, u64)> {
    static WIN: OnceLock<Option<(u64, u64)>> = OnceLock::new();
    *WIN.get_or_init(|| {
        let parse = |k: &str| {
            std::env::var(k)
                .ok()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        };
        Some((parse("X86JIT_LOCKSTEP_LO")?, parse("X86JIT_LOCKSTEP_HI")?))
    })
}

/// Optional record cap from `X86JIT_LOCKSTEP_MAX` (decimal). `None` = unbounded.
fn record_cap() -> Option<u64> {
    static CAP: OnceLock<Option<u64>> = OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("X86JIT_LOCKSTEP_MAX")
            .ok()
            .and_then(|s| s.parse().ok())
    })
}

fn sink() -> Option<&'static Mutex<File>> {
    static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("X86JIT_LOCKSTEP")?;
        File::create(&path).ok().map(Mutex::new)
    })
    .as_ref()
}
