//! Lift: x86 -> IR (§7).
//!
//! Two levels (§7.1): an operand-lowering layer beneath the per-mnemonic lift.
//! Every operand is reduced to a [`Val`] via `lower_read` / `lower_write_target`
//! before an op is emitted; memory operands expand to effective-address arithmetic
//! (the single `effective_address` helper, §17.5) plus `Load`/`Store`.

use iced_x86::{Decoder, DecoderError, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

use crate::ir::{
    BtOp, Cond, FPrec, FlagMask, FloatBinOp, FloatUnOp, IrBlock, IrOp, IrRegion, MemOrder,
    PackedBinOp, RegionCaps, RepKind, RmwOp, StrOp, TempGen, VLogicOp, Val,
};
use crate::memory::Memory;
use crate::state::{iced_gpr_index, Reg};

/// Guest execution mode. Long mode only today; this is the seam (§17.3) that keeps
/// the literal `64` out of the decoder so a 32-bit mode could be added in one place.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CpuMode {
    Long64,
}

impl CpuMode {
    pub fn bits(self) -> u32 {
        match self {
            CpuMode::Long64 => 64,
        }
    }
}

/// A destination a result can be written to (§7.1).
pub enum WriteTarget {
    Reg {
        reg: Reg,
        size: u8,
    },
    Mem {
        addr: Val,
        size: u8,
    },
    /// A high-byte register (AH/BH/CH/DH — bits 8–15 of a GPR). Written by a
    /// read-mask-merge sequence on the parent; not expressible as a `Reg`.
    HighByte {
        parent: Reg,
    },
}

/// Lift errors are mapped to `Exit` in the dispatcher, never to a panic (§7.3).
#[derive(Debug)]
pub enum LiftError {
    /// Decoded by iced, but the lift does not handle it yet.
    Unsupported { addr: u64, bytes: [u8; 15], len: u8 },
    /// Could not even decode (garbage / bytes outside mapped memory).
    DecodeFault { addr: u64 },
}

/// Bytes fetched per block-lift attempt. A block ends at its first control-flow
/// instruction, so this only bounds a *branchless* stretch: one exceeding it is cut
/// at the last complete instruction and falls through to a continuation block (see
/// `lift_block`). 4 KiB (one page) comfortably holds any real basic block.
const BLOCK_FETCH_WINDOW: usize = 4096;

/// Lift a single basic block starting at guest address `start` (§7.3).
///
/// The block ends at the first control-flow instruction (per iced's flow-control
/// classification, not a hand list) or when the mapped code runs out. `TempGen`
/// grows across the whole block. Emits `IrOp::InsnStart` at each instruction
/// boundary so a mid-block trap can set RIP to the faulting instruction (§8, §16).
pub fn lift_block(mem: &Memory, start: u64) -> Result<IrBlock, LiftError> {
    let mode = CpuMode::Long64;
    let code = mem
        .code_slice(start, BLOCK_FETCH_WINDOW)
        .map_err(|_| LiftError::DecodeFault { addr: start })?;
    let mut decoder = Decoder::with_ip(mode.bits(), code, start, DecoderOptions::NONE);

    let mut ops = Vec::new();
    let mut tg = TempGen::new();
    let mut icount = 0u32;
    let mut guest_len = 0u32;
    let mut insn = Instruction::default();

    while decoder.can_decode() {
        decoder.decode_out(&mut insn);
        if insn.is_invalid() {
            // A straight-line block longer than the fetch window truncates its final
            // instruction at the window boundary: iced reports `NoMoreBytes`. When the
            // region still has code past the window (we capped at `CODE_WINDOW`, not the
            // region end, so the slice is full), end the block cleanly at the last
            // complete instruction and fall through — the dispatcher lifts the
            // continuation at `insn.ip()`. Only a genuine bad opcode, or truncation at
            // the true end of mapped code (a short slice), is a real fault. (Go's bignum
            // crypto, e.g. `p521Square`, has >4 KiB branchless stretches — go-caddy.)
            if decoder.last_error() == DecoderError::NoMoreBytes
                && code.len() == BLOCK_FETCH_WINDOW
                && guest_len > 0
            {
                break;
            }
            return Err(LiftError::DecodeFault { addr: insn.ip() });
        }
        icount += 1;
        guest_len += insn.len() as u32;
        ops.push(IrOp::InsnStart {
            guest_addr: insn.ip(),
        });

        let terminated = lift_insn(&insn, &mut ops, &mut tg)
            .map_err(|e| refill_unsupported_bytes(e, code, start))?;
        if terminated {
            break;
        }
    }

    elide_dead_flags(&mut ops);

    Ok(IrBlock {
        guest_start: start,
        ops,
        temp_count: tg.count(),
        guest_len,
        icount,
    })
}

/// Lift exactly one instruction at `start` into a single-instruction block (§5.2,
/// M4-T10). The dispatcher uses this to single-step the interpreter over an
/// instruction the JIT deferred (an MMIO access): the interpreter re-executes just
/// that instruction — trapping out, or consuming a pending MMIO value/ack on resume
/// — then hands control back to compiled code.
pub fn lift_one(mem: &Memory, start: u64) -> Result<IrBlock, LiftError> {
    let mode = CpuMode::Long64;
    let code = mem
        .code_slice(start, BLOCK_FETCH_WINDOW)
        .map_err(|_| LiftError::DecodeFault { addr: start })?;
    let mut decoder = Decoder::with_ip(mode.bits(), code, start, DecoderOptions::NONE);

    let mut ops = Vec::new();
    let mut tg = TempGen::new();
    let mut insn = Instruction::default();
    if !decoder.can_decode() {
        return Err(LiftError::DecodeFault { addr: start });
    }
    decoder.decode_out(&mut insn);
    if insn.is_invalid() {
        return Err(LiftError::DecodeFault { addr: insn.ip() });
    }
    ops.push(IrOp::InsnStart {
        guest_addr: insn.ip(),
    });
    lift_insn(&insn, &mut ops, &mut tg).map_err(|e| refill_unsupported_bytes(e, code, start))?;
    elide_dead_flags(&mut ops);

    Ok(IrBlock {
        guest_start: start,
        ops,
        temp_count: tg.count(),
        guest_len: insn.len() as u32,
        icount: 1,
    })
}

/// The static (`Val::Imm`) successor addresses of a block: an unconditional jump's
/// target, or a conditional branch's two arms. Indirect jumps / call / ret / etc.
/// have no static successors (their edges leave the region).
fn static_succs(block: &IrBlock) -> Vec<u64> {
    match block.ops.last() {
        Some(IrOp::Jump {
            target: Val::Imm(t),
        }) => vec![*t],
        Some(IrOp::Branch {
            taken, fallthrough, ..
        }) => vec![*taken, *fallthrough],
        _ => vec![],
    }
}

/// Lift a **superblock region** (§12 M5-T3): the entry block and all blocks
/// reachable from it by **static** control-flow edges (unconditional jumps and both
/// arms of conditional branches), up to the caps. Indirect jumps, calls, rets,
/// syscalls, and `hlt` end the region (their edges become normal exits at codegen);
/// so do edges to already-lifted blocks (whether a merge or a back-edge — codegen
/// classifies them by reverse-post-order). A lift error on the *entry* propagates;
/// on any *successor* it just drops that edge to an exit. Blocks are returned in
/// **reverse post-order** (`blocks[0]` is the entry), which lets codegen internalize
/// exactly the forward/merge edges and route back-edges (loops) out to the
/// dispatcher — so this one former serves the straight-line (T3b), DAG (T3c), and
/// loop (T3d) phases; only the codegen's edge handling grows.
pub fn lift_region(mem: &Memory, entry: u64, caps: RegionCaps) -> Result<IrRegion, LiftError> {
    use std::collections::HashMap;

    // DFS from the entry, lifting each block once, collecting a post-order.
    fn dfs(
        mem: &Memory,
        addr: u64,
        caps: RegionCaps,
        blocks: &mut HashMap<u64, IrBlock>,
        post: &mut Vec<u64>,
        icount: &mut u32,
    ) {
        for s in static_succs(&blocks[&addr]) {
            if blocks.contains_key(&s) {
                continue; // already in region — merge/back edge, classified at codegen
            }
            if blocks.len() >= caps.max_blocks || *icount >= caps.max_icount {
                continue; // cap reached — this edge stays an exit
            }
            if let Ok(b) = lift_block(mem, s) {
                *icount += b.icount;
                blocks.insert(s, b);
                dfs(mem, s, caps, blocks, post, icount);
            }
            // an unliftable successor simply stays an exit edge
        }
        post.push(addr); // finished: post-order
    }

    let first = lift_block(mem, entry)?;
    let mut icount = first.icount;
    let mut blocks = HashMap::from([(entry, first)]);
    let mut post = Vec::new();
    dfs(mem, entry, caps, &mut blocks, &mut post, &mut icount);

    // Reverse post-order (entry first). Remove from the map in this order so each
    // `IrBlock` moves out exactly once.
    let ordered: Vec<IrBlock> = post
        .into_iter()
        .rev()
        .map(|a| blocks.remove(&a).unwrap())
        .collect();

    // A back-edge is an in-region static successor at an equal-or-earlier RPO index
    // (a self-loop or an ancestor). Only regions with one iterate enough to pay off.
    let index: HashMap<u64, usize> = ordered
        .iter()
        .enumerate()
        .map(|(i, b)| (b.guest_start, i))
        .collect();
    let has_loop = ordered.iter().enumerate().any(|(i, b)| {
        static_succs(b)
            .iter()
            .any(|s| index.get(s).is_some_and(|&j| j <= i))
    });

    Ok(IrRegion {
        entry,
        blocks: ordered,
        has_loop,
    })
}

// Flag bit positions in a `FlagMask` (matches `store_flags` order): CF PF AF ZF SF OF.
const F_CF: u8 = 1 << 0;
const F_PF: u8 = 1 << 1;
const F_AF: u8 = 1 << 2;
const F_ZF: u8 = 1 << 3;
const F_SF: u8 = 1 << 4;
const F_OF: u8 = 1 << 5;
// AF is written by ALU ops but no condition code reads it (only `daa`/`aaa`, which
// the lift doesn't cover) — kept for a complete bit map.
const _: u8 = F_AF;

/// Which flags a condition code inspects.
fn cond_reads(cond: Cond) -> u8 {
    use Cond::*;
    match cond {
        Eq | Ne => F_ZF,
        Below | AboveEq => F_CF,
        BelowEq | Above => F_CF | F_ZF,
        Less | GreaterEq => F_SF | F_OF,
        LessEq | Greater => F_SF | F_OF | F_ZF,
        Sign | NoSign => F_SF,
        Overflow | NoOverflow => F_OF,
        Parity | NoParity => F_PF,
    }
}

/// Flags an op *reads*. The IR has exactly four flag consumers: `Branch`/`GetCond`
/// (a condition code) and `Adc`/`Sbb` (carry-in). No `lahf`/`sahf`/`pushf`/`rcl`
/// exist to read the whole set, so this enumeration is complete — keep it in sync
/// if a flag-reading op is ever added.
fn op_reads(op: &IrOp) -> u8 {
    match op {
        IrOp::Branch { cond, .. } | IrOp::GetCond { cond, .. } => cond_reads(*cond),
        IrOp::Adc { .. } | IrOp::Sbb { .. } | IrOp::Rcl { .. } | IrOp::Rcr { .. } => F_CF,
        _ => 0,
    }
}

/// The mutable flag-write mask of an op that carries one (the ALU ops).
fn op_set_flags_mut(op: &mut IrOp) -> Option<&mut FlagMask> {
    use IrOp::*;
    match op {
        Add { set_flags, .. }
        | Adc { set_flags, .. }
        | Sub { set_flags, .. }
        | Sbb { set_flags, .. }
        | And { set_flags, .. }
        | Or { set_flags, .. }
        | Xor { set_flags, .. }
        | Shl { set_flags, .. }
        | Shr { set_flags, .. }
        | Sar { set_flags, .. }
        | Rol { set_flags, .. }
        | Ror { set_flags, .. }
        | Rcl { set_flags, .. }
        | Rcr { set_flags, .. }
        | DoubleShift { set_flags, .. }
        | Mul { set_flags, .. } => Some(set_flags),
        _ => None,
    }
}

/// Dead-flag elimination (spec §3.2, M5-T2 — the compile-time form of "lazy
/// flags"): narrow each ALU op's `set_flags` to only the flags still *live* at that
/// point. A flag written but overwritten before any read is dead; dropping it from
/// the mask lets the backend's flag computation for it (parity, AF, OF …) fall out
/// as dead code. The last writer of each flag before the block ends is always kept
/// (all flags are conservatively live at the boundary), so the observable flag
/// state at every block exit is unchanged — interp == JIT == Unicorn still holds.
fn elide_dead_flags(ops: &mut [IrOp]) {
    let mut live: u8 = 0b11_1111; // all flags live-out at the block boundary
    for op in ops.iter_mut().rev() {
        let reads = op_reads(op);
        if let Some(mask) = op_set_flags_mut(op) {
            mask.0 &= live; // keep only the still-live flags this op writes
            let writes = mask.0; // effective writes after narrowing
            live = (live & !writes) | reads;
        } else {
            live |= reads;
        }
    }
}

/// The reg-or-memory source trichotomy every 2-/3-operand vector lift repeats: operand
/// `$idx` is a register (`|$b|` arm), a memory reference (`|$addr|` arm — the effective
/// address is computed once, §7.1), or unsupported. `$ext` picks the register file
/// (`reg_xmm`/`reg_ymm`/…). The per-op emits stay at the call site; only the guard, the
/// compute-address-once, and the error path live here. Defined before `lift_insn` so the
/// dispatch match can use it too (a `macro_rules!` must precede its use sites).
macro_rules! vec_src_dispatch {
    ($insn:expr, $ops:expr, $tg:expr, $ext:ident, $idx:expr,
     |$b:ident| $reg:expr, |$addr:ident| $mem:expr $(,)?) => {
        match $ext($insn, $idx) {
            Some($b) => {
                $reg;
            }
            None if $insn.op_kind($idx) == OpKind::Memory => {
                let $addr = effective_address($insn, $ops, $tg)?;
                $mem;
            }
            None => return Err(unsupported_insn($insn)),
        }
    };
}

/// Lift one instruction; returns `true` if it ends the block (control flow).
fn lift_insn(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<bool, LiftError> {
    use Mnemonic::*;
    match insn.mnemonic() {
        // No architectural effect for our purposes (CET markers, pause hint).
        // CET markers / hints that are no-ops without shadow stacks: endbr, pause,
        // and rdssp (leaves its register — glibc's `xor eax; rdsspq rax; test`
        // then correctly detects "no shadow stack"). Prefetch (`0F 18`, `0F 0D`) is a
        // pure cache hint with no architectural effect (Go's runtime memmove emits it).
        Nop | Endbr64 | Endbr32 | Pause | Rdsspd | Rdsspq | Prefetchnta | Prefetcht0
        | Prefetcht1 | Prefetcht2 | Prefetchw | Prefetchwt1 => Ok(false),

        Mov => {
            let src = lower_read(insn, 1, ops, tg)?;
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, src);
            Ok(false)
        }
        Lea => {
            // Address arithmetic only — no Load, and the segment base is ignored:
            // `lea rax, fs:[rbx]` yields `rbx`, not `rbx + fs_base`. So compute the
            // offset via the no-segment path (§16).
            let addr = effective_address_no_segment(insn, ops, tg)?;
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, addr);
            Ok(false)
        }

        Add => lift_binop(insn, ops, tg, BinOp::Add, FlagMask::ALL, true).map(|_| false),
        Adc => lift_binop(insn, ops, tg, BinOp::Adc, FlagMask::ALL, true).map(|_| false),
        Sub => lift_binop(insn, ops, tg, BinOp::Sub, FlagMask::ALL, true).map(|_| false),
        Sbb => lift_binop(insn, ops, tg, BinOp::Sbb, FlagMask::ALL, true).map(|_| false),
        And => lift_binop(insn, ops, tg, BinOp::And, FlagMask::ALL, true).map(|_| false),
        Or => lift_binop(insn, ops, tg, BinOp::Or, FlagMask::ALL, true).map(|_| false),
        Xor => lift_binop(insn, ops, tg, BinOp::Xor, FlagMask::ALL, true).map(|_| false),
        // cmp / test set flags but discard the result (no write-back).
        Cmp => lift_binop(insn, ops, tg, BinOp::Sub, FlagMask::ALL, false).map(|_| false),
        Test => lift_binop(insn, ops, tg, BinOp::And, FlagMask::ALL, false).map(|_| false),

        // shifts: count-conditional flags (§16), AF undefined → FlagMask::SHIFT.
        Shl => lift_binop(insn, ops, tg, BinOp::Shl, FlagMask::SHIFT, true).map(|_| false),
        Shr => lift_binop(insn, ops, tg, BinOp::Shr, FlagMask::SHIFT, true).map(|_| false),
        Sar => lift_binop(insn, ops, tg, BinOp::Sar, FlagMask::SHIFT, true).map(|_| false),
        // rotates: only CF/OF, count-conditional (CF_OF mask).
        Rol => lift_binop(insn, ops, tg, BinOp::Rol, FlagMask::CF_OF, true).map(|_| false),
        Ror => lift_binop(insn, ops, tg, BinOp::Ror, FlagMask::CF_OF, true).map(|_| false),
        // rotate-through-carry: like the rotates but consume CF (§16, task-132).
        Rcl => lift_binop(insn, ops, tg, BinOp::Rcl, FlagMask::CF_OF, true).map(|_| false),
        Rcr => lift_binop(insn, ops, tg, BinOp::Rcr, FlagMask::CF_OF, true).map(|_| false),
        // double-precision shifts (SHLD/SHRD): shift op0 by count, fill from op1.
        Shld => lift_double_shift(insn, ops, tg, true).map(|_| false),
        Shrd => lift_double_shift(insn, ops, tg, false).map(|_| false),

        // inc/dec keep CF (ALL_BUT_CF); neg is 0 - operand; not is bitwise, no flags.
        Inc => lift_incdec(insn, ops, tg, BinOp::Add).map(|_| false),
        Dec => lift_incdec(insn, ops, tg, BinOp::Sub).map(|_| false),
        Neg => lift_neg(insn, ops, tg).map(|_| false),
        Not => lift_not(insn, ops, tg).map(|_| false),

        Mul => lift_widening_mul(insn, ops, tg, false).map(|_| false),
        Imul => lift_imul(insn, ops, tg).map(|_| false),
        Div => lift_div(insn, ops, tg, false).map(|_| false),
        Idiv => lift_div(insn, ops, tg, true).map(|_| false),

        Bswap => lift_bswap(insn, ops, tg).map(|_| false),
        Movbe => lift_movbe(insn, ops, tg).map(|_| false),
        // BMI1/BMI2 single-dst family (task-168.5.3). sarx/shlx/shrx/rorx/mulx/pdep/pext
        // are a follow-up (shift-reuse / two-dst / helper).
        Andn => lift_bmi(insn, ops, tg, crate::ir::BmiOp::Andn).map(|_| false),
        Blsi => lift_bmi(insn, ops, tg, crate::ir::BmiOp::Blsi).map(|_| false),
        Blsr => lift_bmi(insn, ops, tg, crate::ir::BmiOp::Blsr).map(|_| false),
        Blsmsk => lift_bmi(insn, ops, tg, crate::ir::BmiOp::Blsmsk).map(|_| false),
        Bextr => lift_bmi(insn, ops, tg, crate::ir::BmiOp::Bextr).map(|_| false),
        Bzhi => lift_bmi(insn, ops, tg, crate::ir::BmiOp::Bzhi).map(|_| false),
        // BMI2 flagless shifts/rotate — reuse the existing shift/rotate IR ops.
        Shlx => lift_bmi_shift(insn, ops, tg, |dst, a, b, size, set_flags| IrOp::Shl {
            dst,
            a,
            b,
            size,
            set_flags,
        })
        .map(|_| false),
        Shrx => lift_bmi_shift(insn, ops, tg, |dst, a, b, size, set_flags| IrOp::Shr {
            dst,
            a,
            b,
            size,
            set_flags,
        })
        .map(|_| false),
        Sarx => lift_bmi_shift(insn, ops, tg, |dst, a, b, size, set_flags| IrOp::Sar {
            dst,
            a,
            b,
            size,
            set_flags,
        })
        .map(|_| false),
        Rorx => lift_bmi_shift(insn, ops, tg, |dst, a, b, size, set_flags| IrOp::Ror {
            dst,
            a,
            b,
            size,
            set_flags,
        })
        .map(|_| false),
        Pdep => lift_bmi(insn, ops, tg, crate::ir::BmiOp::Pdep).map(|_| false),
        Pext => lift_bmi(insn, ops, tg, crate::ir::BmiOp::Pext).map(|_| false),
        Mulx => lift_mulx(insn, ops, tg).map(|_| false),
        Xchg => lift_xchg(insn, ops, tg).map(|_| false),
        Xadd => lift_xadd(insn, ops, tg).map(|_| false),
        Cmpxchg => lift_cmpxchg(insn, ops, tg).map(|_| false),
        Cpuid => {
            ops.push(IrOp::Cpuid);
            Ok(false)
        }
        // xgetbv: EDX:EAX = the extended control register selected by ECX. Guests read
        // XCR0 (ECX=0) after seeing CPUID.1.ECX.OSXSAVE. Runtime op so XCR0 tracks the
        // embedder's feature set (task-169) instead of a baked constant.
        Xgetbv => {
            ops.push(IrOp::Xgetbv);
            Ok(false)
        }
        // rdtsc: a fixed timestamp keeps whole-program runs deterministic (§14).
        // EDX:EAX = counter; both writes zero the upper 32 bits of their register.
        Rdtsc => {
            ops.push(IrOp::WriteReg {
                reg: Reg::Rax,
                src: Val::Imm(0x1234_5678),
                size: 4,
            });
            ops.push(IrOp::WriteReg {
                reg: Reg::Rdx,
                src: Val::Imm(0),
                size: 4,
            });
            Ok(false)
        }
        Fld | Fst | Fstp | Fild | Fistp | Fadd | Faddp | Fsub | Fsubp | Fsubr | Fsubrp | Fmul
        | Fmulp | Fdiv | Fdivp | Fdivr | Fdivrp | Fld1 | Fldz | Fabs | Fchs | Fxch | Fucomi
        | Fucomip | Fcomi | Fcomip | Fldcw | Fnstcw | Fnstsw | Fprem => {
            lift_x87(insn, ops, tg).map(|_| false)
        }
        Bsf => lift_bitscan(insn, ops, tg, crate::ir::BitScanOp::Bsf).map(|_| false),
        Bsr => lift_bitscan(insn, ops, tg, crate::ir::BitScanOp::Bsr).map(|_| false),
        Tzcnt => lift_bitscan(insn, ops, tg, crate::ir::BitScanOp::Tzcnt).map(|_| false),
        Lzcnt => lift_bitscan(insn, ops, tg, crate::ir::BitScanOp::Lzcnt).map(|_| false),
        Popcnt => {
            let size = operand_size(insn, 0);
            let src = lower_read(insn, 1, ops, tg)?;
            let t = tg.fresh();
            ops.push(IrOp::Popcnt { dst: t, src, size });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(t));
            Ok(false)
        }
        Crc32 => {
            let crc = lower_read(insn, 0, ops, tg)?;
            let src = lower_read(insn, 1, ops, tg)?;
            let bytes = operand_size(insn, 1);
            let t = tg.fresh();
            ops.push(IrOp::Crc32 {
                dst: t,
                crc,
                src,
                bytes,
            });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(t));
            Ok(false)
        }
        // MXCSR isn't modeled (default round-to-nearest, exceptions masked):
        // stmxcsr writes that default; ldmxcsr is ignored.
        Stmxcsr => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::Store {
                addr,
                src: Val::Imm(0x1F80),
                size: 4,
                order: MemOrder::None,
            });
            Ok(false)
        }
        Ldmxcsr => Ok(false),
        // fxsave/fxrstor: 512-byte legacy FP/SSE save area. Shared exec_fxstate in
        // both backends (glibc's dynamic loader fxsaves to preserve XMM across the
        // PLT resolver when XSAVE isn't advertised).
        Fxsave | Fxsave64 => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::FxState {
                addr,
                restore: false,
            });
            Ok(false)
        }
        Fxrstor | Fxrstor64 => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::FxState {
                addr,
                restore: true,
            });
            Ok(false)
        }
        // Fences: single-threaded ordering is already TSO; no-op (§8.2.3).
        Mfence | Lfence | Sfence => Ok(false),
        Bt => lift_bt(insn, ops, tg, BtOp::Test).map(|_| false),
        Bts => lift_bt(insn, ops, tg, BtOp::Set).map(|_| false),
        Btr => lift_bt(insn, ops, tg, BtOp::Reset).map(|_| false),
        Btc => lift_bt(insn, ops, tg, BtOp::Complement).map(|_| false),

        // --- string ops + direction flag (§10) ---
        Std => {
            ops.push(IrOp::SetDf { value: true });
            Ok(false)
        }
        Cld => {
            ops.push(IrOp::SetDf { value: false });
            Ok(false)
        }
        Movsb => lift_string(insn, ops, StrOp::Movs, 1),
        Movsw => lift_string(insn, ops, StrOp::Movs, 2),
        Movsq => lift_string(insn, ops, StrOp::Movs, 8),
        Stosb => lift_string(insn, ops, StrOp::Stos, 1),
        Stosw => lift_string(insn, ops, StrOp::Stos, 2),
        Stosd => lift_string(insn, ops, StrOp::Stos, 4),
        Stosq => lift_string(insn, ops, StrOp::Stos, 8),
        Lodsb => lift_string(insn, ops, StrOp::Lods, 1),
        Lodsw => lift_string(insn, ops, StrOp::Lods, 2),
        Lodsd => lift_string(insn, ops, StrOp::Lods, 4),
        Lodsq => lift_string(insn, ops, StrOp::Lods, 8),
        Scasb => lift_string(insn, ops, StrOp::Scas, 1),
        Scasw => lift_string(insn, ops, StrOp::Scas, 2),
        Scasd => lift_string(insn, ops, StrOp::Scas, 4),
        Scasq => lift_string(insn, ops, StrOp::Scas, 8),
        Cmpsb => lift_string(insn, ops, StrOp::Cmps, 1),
        Cmpsw => lift_string(insn, ops, StrOp::Cmps, 2),
        Cmpsq => lift_string(insn, ops, StrOp::Cmps, 8),
        // Movsd/Cmpsd/Movss... also name SSE scalar moves — route the memory-operand
        // (string) form here, defer the xmm form.
        Movsd if reg_xmm(insn, 0).is_none() && reg_xmm(insn, 1).is_none() => {
            lift_string(insn, ops, StrOp::Movs, 4)
        }
        Cmpsd if reg_xmm(insn, 0).is_none() && reg_xmm(insn, 1).is_none() => {
            lift_string(insn, ops, StrOp::Cmps, 4)
        }
        // xmm form: compare-scalar-double with a predicate imm.
        Cmpsd => lift_float_cmp_mask(insn, ops, FPrec::F64, true).map(|_| false),

        // --- SSE data movement + logic (§3.1 M8) ---
        Movdqa | Movdqu | Movaps | Movups | Movapd | Movupd => {
            lift_vmov(insn, ops, tg, 16).map(|_| false)
        }
        Movq => lift_vmov(insn, ops, tg, 8).map(|_| false),
        Movd => lift_vmov(insn, ops, tg, 4).map(|_| false),
        Movlhps => lift_move_half(insn, ops, true, false).map(|_| false),
        Movhlps => lift_move_half(insn, ops, false, true).map(|_| false),
        Movhps | Movhpd => lift_half_mem(insn, ops, tg, true).map(|_| false),
        Movlps | Movlpd => lift_half_mem(insn, ops, tg, false).map(|_| false),
        Pxor | Xorps | Xorpd => lift_vlogic(insn, ops, tg, VLogicOp::Xor).map(|_| false),
        Pand | Andps | Andpd => lift_vlogic(insn, ops, tg, VLogicOp::And).map(|_| false),
        Por | Orps | Orpd => lift_vlogic(insn, ops, tg, VLogicOp::Or).map(|_| false),
        Pandn | Andnps | Andnpd => lift_vlogic(insn, ops, tg, VLogicOp::Andn).map(|_| false),

        // packed integer arithmetic (register source only for now)
        Paddb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::Add).map(|_| false),
        Paddw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::Add).map(|_| false),
        Paddd => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::Add).map(|_| false),
        Paddq => lift_vpacked_bin(insn, ops, tg, 8, PackedBinOp::Add).map(|_| false),
        Psubb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::Sub).map(|_| false),
        Psubw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::Sub).map(|_| false),
        Psubd => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::Sub).map(|_| false),
        Psubq => lift_vpacked_bin(insn, ops, tg, 8, PackedBinOp::Sub).map(|_| false),
        Pcmpeqb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::CmpEq).map(|_| false),
        Pcmpeqw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::CmpEq).map(|_| false),
        Pcmpeqd => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::CmpEq).map(|_| false),
        Pcmpgtb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::CmpGt).map(|_| false),
        Pcmpgtw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::CmpGt).map(|_| false),
        Pcmpgtd => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::CmpGt).map(|_| false),
        Pcmpgtq => lift_vpacked_bin(insn, ops, tg, 8, PackedBinOp::CmpGt).map(|_| false),
        Pcmpeqq => lift_vpacked_bin(insn, ops, tg, 8, PackedBinOp::CmpEq).map(|_| false),
        Pminub => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::MinU).map(|_| false),
        Pmaxub => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::MaxU).map(|_| false),
        Pminsw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::MinS).map(|_| false),
        Pmaxsw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::MaxS).map(|_| false),
        // packed shift by immediate
        Psllw => lift_vpacked_shift(insn, ops, 2, false, false).map(|_| false),
        Pslld => lift_vpacked_shift(insn, ops, 4, false, false).map(|_| false),
        Psllq => lift_vpacked_shift(insn, ops, 8, false, false).map(|_| false),
        Psrlw => lift_vpacked_shift(insn, ops, 2, true, false).map(|_| false),
        Psrld => lift_vpacked_shift(insn, ops, 4, true, false).map(|_| false),
        Psrlq => lift_vpacked_shift(insn, ops, 8, true, false).map(|_| false),
        Psraw => lift_vpacked_shift(insn, ops, 2, true, true).map(|_| false),
        Psrad => lift_vpacked_shift(insn, ops, 4, true, true).map(|_| false),
        Psrldq => lift_byteshift(insn, ops, true).map(|_| false),
        Pslldq => lift_byteshift(insn, ops, false).map(|_| false),

        // shuffles / unpacks / pack / insert
        Pshufd => lift_pshufd(insn, ops, tg).map(|_| false),
        Pshuflw => lift_pshufw(insn, ops, false).map(|_| false),
        Pshufhw => lift_pshufw(insn, ops, true).map(|_| false),
        Shufps | Shufpd => lift_shufps(insn, ops).map(|_| false),
        Pshufb => {
            let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            vec_src_dispatch!(
                insn,
                ops,
                tg,
                reg_xmm,
                1,
                |idx| ops.push(IrOp::VPshufb { dst: d, idx }),
                |addr| ops.push(IrOp::VPshufbM { dst: d, addr })
            );
            Ok(false)
        }
        Palignr => {
            let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let imm = insn.immediate(2) as u8;
            vec_src_dispatch!(
                insn,
                ops,
                tg,
                reg_xmm,
                1,
                |src| ops.push(IrOp::VAlignr { dst: d, src, imm }),
                |addr| ops.push(IrOp::VAlignrM { dst: d, addr, imm })
            );
            Ok(false)
        }
        Punpcklbw => lift_vunpack(insn, ops, 1, false).map(|_| false),
        Punpcklwd => lift_vunpack(insn, ops, 2, false).map(|_| false),
        Punpckldq => lift_vunpack(insn, ops, 4, false).map(|_| false),
        Punpcklqdq => lift_vunpack(insn, ops, 8, false).map(|_| false),
        Punpckhbw => lift_vunpack(insn, ops, 1, true).map(|_| false),
        Punpckhwd => lift_vunpack(insn, ops, 2, true).map(|_| false),
        Punpckhdq => lift_vunpack(insn, ops, 4, true).map(|_| false),
        Punpckhqdq => lift_vunpack(insn, ops, 8, true).map(|_| false),
        Packuswb => lift_packuswb(insn, ops).map(|_| false),
        Pinsrw => lift_pinsrw(insn, ops, tg).map(|_| false),
        Pextrw | Vpextrw => lift_pextrw(insn, ops, tg).map(|_| false),
        Pextrb | Vpextrb => lift_pextr(insn, ops, tg, 1).map(|_| false),
        Pextrd | Vpextrd => lift_pextr(insn, ops, tg, 4).map(|_| false),
        Pextrq | Vpextrq => lift_pextr(insn, ops, tg, 8).map(|_| false),
        // pinsrb/d/q + VEX vpinsr{b,w,d,q} (task-168.5 grind).
        Pinsrb | Vpinsrb => lift_pinsr(insn, ops, tg, 1).map(|_| false),
        Vpinsrw => lift_pinsr(insn, ops, tg, 2).map(|_| false),
        Pinsrd | Vpinsrd => lift_pinsr(insn, ops, tg, 4).map(|_| false),
        Pinsrq | Vpinsrq => lift_pinsr(insn, ops, tg, 8).map(|_| false),
        Pmovmskb => {
            let src = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let t = tg.fresh();
            ops.push(IrOp::VMoveMaskB { dst: t, src });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(t));
            Ok(false)
        }

        // --- AVX (VEX.128) — task-168.1/168.2. Reuse the u128 vector IR (already
        // 3-operand `dst,a,b`); a register destination also clears bits 255:128 of the
        // YMM via `VZeroUpper` (task-168.2). 256-bit/YMM forms fall through to
        // `unsupported` (`reg_xmm` rejects YMM) — deferred to AVX-256. ---
        // VEX forms (no EVEX mask) — `elem` is unused on the unmasked path, pass 4.
        Vmovdqa | Vmovdqu | Vmovaps | Vmovups | Vmovapd | Vmovupd => {
            lift_vmov_avx(insn, ops, tg, 4).map(|_| false)
        }
        // EVEX data movement (task-168.5 unmasked / task-170.1 masked). The element
        // suffix is the write-mask granularity: 8/16/32/64 → 1/2/4/8 bytes.
        Vmovdqu8 => lift_vmov_avx(insn, ops, tg, 1).map(|_| false),
        Vmovdqu16 => lift_vmov_avx(insn, ops, tg, 2).map(|_| false),
        Vmovdqu32 | Vmovdqa32 => lift_vmov_avx(insn, ops, tg, 4).map(|_| false),
        Vmovdqu64 | Vmovdqa64 => lift_vmov_avx(insn, ops, tg, 8).map(|_| false),
        Vmovq => lift_vmov_vex(insn, ops, tg, 8).map(|_| false),
        Vmovd => lift_vmov_vex(insn, ops, tg, 4).map(|_| false),
        Vpxor | Vxorps | Vxorpd => lift_vlogic_avx(insn, ops, tg, VLogicOp::Xor).map(|_| false),
        Vpand | Vandps | Vandpd => lift_vlogic_avx(insn, ops, tg, VLogicOp::And).map(|_| false),
        Vpor | Vorps | Vorpd => lift_vlogic_avx(insn, ops, tg, VLogicOp::Or).map(|_| false),
        Vpandn | Vandnps | Vandnpd => lift_vlogic_avx(insn, ops, tg, VLogicOp::Andn).map(|_| false),
        // EVEX bitwise logic (task-168.5.2): width-generic 128/256/512, unmasked.
        Vpxord | Vpxorq => lift_evex_vlogic(insn, ops, VLogicOp::Xor).map(|_| false),
        Vpandd | Vpandq => lift_evex_vlogic(insn, ops, VLogicOp::And).map(|_| false),
        Vpord | Vporq => lift_evex_vlogic(insn, ops, VLogicOp::Or).map(|_| false),
        Vpandnd | Vpandnq => lift_evex_vlogic(insn, ops, VLogicOp::Andn).map(|_| false),
        Vpternlogd | Vpternlogq => lift_vpternlog(insn, ops).map(|_| false),
        // SSE4.1 pmovzx/pmovsx (task-168.5.4): zero/sign-extend narrow → wide lanes.
        Pmovzxbw => lift_pmovx(insn, ops, tg, 1, 2, false).map(|_| false),
        Pmovzxbd => lift_pmovx(insn, ops, tg, 1, 4, false).map(|_| false),
        Pmovzxbq => lift_pmovx(insn, ops, tg, 1, 8, false).map(|_| false),
        Pmovzxwd => lift_pmovx(insn, ops, tg, 2, 4, false).map(|_| false),
        Pmovzxwq => lift_pmovx(insn, ops, tg, 2, 8, false).map(|_| false),
        Pmovzxdq => lift_pmovx(insn, ops, tg, 4, 8, false).map(|_| false),
        Pmovsxbw => lift_pmovx(insn, ops, tg, 1, 2, true).map(|_| false),
        Pmovsxbd => lift_pmovx(insn, ops, tg, 1, 4, true).map(|_| false),
        Pmovsxbq => lift_pmovx(insn, ops, tg, 1, 8, true).map(|_| false),
        Pmovsxwd => lift_pmovx(insn, ops, tg, 2, 4, true).map(|_| false),
        Pmovsxwq => lift_pmovx(insn, ops, tg, 2, 8, true).map(|_| false),
        Pmovsxdq => lift_pmovx(insn, ops, tg, 4, 8, true).map(|_| false),
        // SSE4.1 pmulld: per-lane low 32 bits of the 32×32 product.
        Pmulld => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MulLo32).map(|_| false),
        Vpaddb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::Add).map(|_| false),
        Vpaddw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::Add).map(|_| false),
        Vpaddd => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::Add).map(|_| false),
        Vpaddq => lift_vpacked_bin_avx(insn, ops, tg, 8, PackedBinOp::Add).map(|_| false),
        Vpsubb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::Sub).map(|_| false),
        Vpsubw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::Sub).map(|_| false),
        Vpsubd => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::Sub).map(|_| false),
        Vpsubq => lift_vpacked_bin_avx(insn, ops, tg, 8, PackedBinOp::Sub).map(|_| false),
        // vpcmpeq*/vpcmpgt*: EVEX form (k destination) → opmask; else packed (xmm/ymm).
        // Predicate encoding (ir.rs): 0 = EQ, 6 = GT (signed).
        Vpcmpeqb => lift_vpcmp_fixed_or_packed(insn, ops, tg, 1, PackedBinOp::CmpEq, 0, false)
            .map(|_| false),
        Vpcmpeqw => lift_vpcmp_fixed_or_packed(insn, ops, tg, 2, PackedBinOp::CmpEq, 0, false)
            .map(|_| false),
        Vpcmpeqd => lift_vpcmp_fixed_or_packed(insn, ops, tg, 4, PackedBinOp::CmpEq, 0, false)
            .map(|_| false),
        Vpcmpgtb => {
            lift_vpcmp_fixed_or_packed(insn, ops, tg, 1, PackedBinOp::CmpGt, 6, true).map(|_| false)
        }
        Vpcmpgtw => {
            lift_vpcmp_fixed_or_packed(insn, ops, tg, 2, PackedBinOp::CmpGt, 6, true).map(|_| false)
        }
        Vpcmpgtd => {
            lift_vpcmp_fixed_or_packed(insn, ops, tg, 4, PackedBinOp::CmpGt, 6, true).map(|_| false)
        }
        Vpminub => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::MinU).map(|_| false),
        Vpmaxub => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::MaxU).map(|_| false),
        // EVEX-only 64-bit packed min/max (AVX-512, task-168.5 grind). 128-bit only.
        Vpmaxuq => lift_evex_packed_bin_128(insn, ops, tg, 8, PackedBinOp::MaxU).map(|_| false),
        Vpminuq => lift_evex_packed_bin_128(insn, ops, tg, 8, PackedBinOp::MinU).map(|_| false),
        Vpmaxsq => lift_evex_packed_bin_128(insn, ops, tg, 8, PackedBinOp::MaxS).map(|_| false),
        Vpminsq => lift_evex_packed_bin_128(insn, ops, tg, 8, PackedBinOp::MinS).map(|_| false),
        // EVEX vpcmp{,u}{b,w,d,q} → opmask (task-168.5 opmask subsystem).
        Vpcmpb => lift_vpcmp(insn, ops, 1, true).map(|_| false),
        Vpcmpw => lift_vpcmp(insn, ops, 2, true).map(|_| false),
        Vpcmpd => lift_vpcmp(insn, ops, 4, true).map(|_| false),
        Vpcmpq => lift_vpcmp(insn, ops, 8, true).map(|_| false),
        Vpcmpub => lift_vpcmp(insn, ops, 1, false).map(|_| false),
        Vpcmpuw => lift_vpcmp(insn, ops, 2, false).map(|_| false),
        Vpcmpud => lift_vpcmp(insn, ops, 4, false).map(|_| false),
        Vpcmpuq => lift_vpcmp(insn, ops, 8, false).map(|_| false),
        // Opmask flag tests kortest{b,w,d,q} (task-168.5 opmask subsystem).
        Kortestb => lift_kortest(insn, ops, 8).map(|_| false),
        Kortestw => lift_kortest(insn, ops, 16).map(|_| false),
        Kortestd => lift_kortest(insn, ops, 32).map(|_| false),
        Kortestq => lift_kortest(insn, ops, 64).map(|_| false),
        Kmovb => lift_kmov(insn, ops, tg, 8).map(|_| false),
        Kmovw => lift_kmov(insn, ops, tg, 16).map(|_| false),
        Kmovd => lift_kmov(insn, ops, tg, 32).map(|_| false),
        Kmovq => lift_kmov(insn, ops, tg, 64).map(|_| false),
        Vpmovmskb => {
            let t = tg.fresh();
            if let Some(src) = reg_ymm(insn, 1) {
                ops.push(IrOp::VMoveMaskB256 { dst: t, src }); // 32-byte mask (task-168.2)
            } else {
                let src = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
                ops.push(IrOp::VMoveMaskB { dst: t, src });
            }
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(t));
            Ok(false)
        }
        Vpshufb => {
            // 3-operand `dst = pshufb(op1, op2)`. YMM → the 256-bit per-lane form.
            if let Some(d) = reg_ymm(insn, 0) {
                let a = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
                vec_src_dispatch!(
                    insn,
                    ops,
                    tg,
                    reg_ymm,
                    2,
                    |idx| ops.push(IrOp::VPshufb256 { dst: d, a, idx }),
                    |addr| ops.push(IrOp::VPshufb256M { dst: d, a, addr })
                );
                return Ok(false);
            }
            // VEX.128: `VPshufb` shuffles `dst` in place, so move op1 into dst first.
            let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            if d != a {
                ops.push(IrOp::VMov { dst: d, src: a });
            }
            vec_src_dispatch!(
                insn,
                ops,
                tg,
                reg_xmm,
                2,
                |idx| ops.push(IrOp::VPshufb { dst: d, idx }),
                |addr| ops.push(IrOp::VPshufbM { dst: d, addr })
            );
            ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
            Ok(false)
        }
        Vzeroupper | Vzeroall => {
            ops.push(IrOp::VZeroUpperAll);
            Ok(false)
        }
        // AVX2 broadcast (task-168.3): replicate the low element across the dest.
        Vpbroadcastb => lift_broadcast(insn, ops, tg, 1).map(|_| false),
        Vpbroadcastw => lift_broadcast(insn, ops, tg, 2).map(|_| false),
        Vpbroadcastd => lift_broadcast(insn, ops, tg, 4).map(|_| false),
        Vpbroadcastq => lift_broadcast(insn, ops, tg, 8).map(|_| false),
        // 128-bit lane insert / extract between XMM and YMM (task-168.3).
        Vinserti128 | Vinsertf128 => {
            let dst = reg_ymm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let src = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let ins = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?; // mem src deferred
            let hi = insn.immediate(3) & 1 != 0;
            ops.push(IrOp::VInsert128 { dst, src, ins, hi });
            Ok(false)
        }
        Vextracti128 | Vextractf128 => {
            let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?; // mem dst deferred
            let src = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let hi = insn.immediate(2) & 1 != 0;
            ops.push(IrOp::VExtract128 { dst, src, hi });
            Ok(false)
        }
        // VEX packed shift-by-immediate (128 + 256), task-168.3.
        Vpsllw => lift_vpacked_shift_avx(insn, ops, 2, false, false).map(|_| false),
        Vpslld => lift_vpacked_shift_avx(insn, ops, 4, false, false).map(|_| false),
        Vpsllq => lift_vpacked_shift_avx(insn, ops, 8, false, false).map(|_| false),
        Vpsrlw => lift_vpacked_shift_avx(insn, ops, 2, true, false).map(|_| false),
        Vpsrld => lift_vpacked_shift_avx(insn, ops, 4, true, false).map(|_| false),
        Vpsrlq => lift_vpacked_shift_avx(insn, ops, 8, true, false).map(|_| false),
        Vpsraw => lift_vpacked_shift_avx(insn, ops, 2, true, true).map(|_| false),
        Vpsrad => lift_vpacked_shift_avx(insn, ops, 4, true, true).map(|_| false),

        // AVX2 cross-lane permutes (task-168.3). Register forms; memory sources
        // deferred (mirrors vinserti128).
        Vpermq if insn.op_kind(2) == OpKind::Immediate8 => {
            let dst = reg_ymm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let src = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let imm = insn.immediate8();
            ops.push(IrOp::VPermq { dst, src, imm });
            Ok(false)
        }
        Vpermd => {
            let dst = reg_ymm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let ctrl = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let src = reg_ymm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
            ops.push(IrOp::VPermd { dst, ctrl, src });
            Ok(false)
        }
        Vperm2i128 | Vperm2f128 => {
            let dst = reg_ymm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let a = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let b = reg_ymm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
            let imm = insn.immediate(3) as u8;
            ops.push(IrOp::VPerm2i128 { dst, a, b, imm });
            Ok(false)
        }
        // AVX `vptest` (+ legacy `ptest`): flags-only AND test. op0 = DEST, op1 =
        // SRC. YMM → 256-bit form (task-168.4). Register src; memory deferred.
        Ptest | Vptest => {
            if let Some(a) = reg_ymm(insn, 0) {
                let b = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
                ops.push(IrOp::VPtest { a, b, w256: true });
                return Ok(false);
            }
            let a = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            ops.push(IrOp::VPtest { a, b, w256: false });
            Ok(false)
        }
        Vpalignr => {
            let imm = insn.immediate(3) as u8;
            // YMM → per-lane 256-bit form; VEX.128 → 3-operand in-place align.
            if let Some(dst) = reg_ymm(insn, 0) {
                let a = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
                let b = reg_ymm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
                ops.push(IrOp::VPalignr256 { dst, a, b, imm });
                return Ok(false);
            }
            let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let b = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
            if d != a {
                ops.push(IrOp::VMov { dst: d, src: a });
            }
            ops.push(IrOp::VAlignr {
                dst: d,
                src: b,
                imm,
            });
            ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
            Ok(false)
        }

        // --- SSE/SSE2 floating point (§3.1 M8) ---
        // Scalar float move (xmm forms; the mem `Movsd` string form is handled above).
        Movss => lift_scalar_fmove(insn, ops, tg, FPrec::F32).map(|_| false),
        Movsd => lift_scalar_fmove(insn, ops, tg, FPrec::F64).map(|_| false),
        Addss => lift_float_bin(insn, ops, tg, FloatBinOp::Add, FPrec::F32, true).map(|_| false),
        Addsd => lift_float_bin(insn, ops, tg, FloatBinOp::Add, FPrec::F64, true).map(|_| false),
        Addps => lift_float_bin(insn, ops, tg, FloatBinOp::Add, FPrec::F32, false).map(|_| false),
        Addpd => lift_float_bin(insn, ops, tg, FloatBinOp::Add, FPrec::F64, false).map(|_| false),
        Subss => lift_float_bin(insn, ops, tg, FloatBinOp::Sub, FPrec::F32, true).map(|_| false),
        Subsd => lift_float_bin(insn, ops, tg, FloatBinOp::Sub, FPrec::F64, true).map(|_| false),
        Subps => lift_float_bin(insn, ops, tg, FloatBinOp::Sub, FPrec::F32, false).map(|_| false),
        Subpd => lift_float_bin(insn, ops, tg, FloatBinOp::Sub, FPrec::F64, false).map(|_| false),
        Mulss => lift_float_bin(insn, ops, tg, FloatBinOp::Mul, FPrec::F32, true).map(|_| false),
        Mulsd => lift_float_bin(insn, ops, tg, FloatBinOp::Mul, FPrec::F64, true).map(|_| false),
        Mulps => lift_float_bin(insn, ops, tg, FloatBinOp::Mul, FPrec::F32, false).map(|_| false),
        Mulpd => lift_float_bin(insn, ops, tg, FloatBinOp::Mul, FPrec::F64, false).map(|_| false),
        Divss => lift_float_bin(insn, ops, tg, FloatBinOp::Div, FPrec::F32, true).map(|_| false),
        Divsd => lift_float_bin(insn, ops, tg, FloatBinOp::Div, FPrec::F64, true).map(|_| false),
        Divps => lift_float_bin(insn, ops, tg, FloatBinOp::Div, FPrec::F32, false).map(|_| false),
        Divpd => lift_float_bin(insn, ops, tg, FloatBinOp::Div, FPrec::F64, false).map(|_| false),
        Ucomiss | Comiss => lift_float_cmp(insn, ops, tg, FPrec::F32).map(|_| false),
        Ucomisd | Comisd => lift_float_cmp(insn, ops, tg, FPrec::F64).map(|_| false),
        Cmpss => lift_float_cmp_mask(insn, ops, FPrec::F32, true).map(|_| false),
        Cmppd => lift_float_cmp_mask(insn, ops, FPrec::F64, false).map(|_| false),
        Cmpps => lift_float_cmp_mask(insn, ops, FPrec::F32, false).map(|_| false),
        Cvtsi2ss => lift_cvt_from_int(insn, ops, tg, FPrec::F32).map(|_| false),
        Cvtsi2sd => lift_cvt_from_int(insn, ops, tg, FPrec::F64).map(|_| false),
        Cvttss2si => lift_cvt_to_int(insn, ops, tg, FPrec::F32, true).map(|_| false),
        Cvtss2si => lift_cvt_to_int(insn, ops, tg, FPrec::F32, false).map(|_| false),
        Cvttsd2si => lift_cvt_to_int(insn, ops, tg, FPrec::F64, true).map(|_| false),
        Cvtsd2si => lift_cvt_to_int(insn, ops, tg, FPrec::F64, false).map(|_| false),
        Cvtss2sd => lift_cvt_float(insn, ops, tg, FPrec::F32, FPrec::F64).map(|_| false),
        Cvtsd2ss => lift_cvt_float(insn, ops, tg, FPrec::F64, FPrec::F32).map(|_| false),
        Minss => lift_float_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F32, true).map(|_| false),
        Minsd => lift_float_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F64, true).map(|_| false),
        Minps => lift_float_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F32, false).map(|_| false),
        Minpd => lift_float_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F64, false).map(|_| false),
        Maxss => lift_float_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F32, true).map(|_| false),
        Maxsd => lift_float_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F64, true).map(|_| false),
        Maxps => lift_float_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F32, false).map(|_| false),
        Maxpd => lift_float_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F64, false).map(|_| false),
        Sqrtss => lift_float_unary(insn, ops, FloatUnOp::Sqrt, FPrec::F32, true).map(|_| false),
        Sqrtsd => lift_float_unary(insn, ops, FloatUnOp::Sqrt, FPrec::F64, true).map(|_| false),
        Sqrtps => lift_float_unary(insn, ops, FloatUnOp::Sqrt, FPrec::F32, false).map(|_| false),
        Sqrtpd => lift_float_unary(insn, ops, FloatUnOp::Sqrt, FPrec::F64, false).map(|_| false),

        Movzx => lift_movzx(insn, ops, tg).map(|_| false),
        Movsx | Movsxd => lift_movsx(insn, ops, tg).map(|_| false),
        Cbw => lift_cbw_family(ops, tg, 1, 2).map(|_| false),
        Cwde => lift_cbw_family(ops, tg, 2, 4).map(|_| false),
        Cdqe => lift_cbw_family(ops, tg, 4, 8).map(|_| false),
        Cwd => lift_sign_into_dx(ops, tg, 2).map(|_| false),
        Cdq => lift_sign_into_dx(ops, tg, 4).map(|_| false),
        Cqo => lift_sign_into_dx(ops, tg, 8).map(|_| false),

        Push => lift_push(insn, ops, tg).map(|_| false),
        Pop => lift_pop(insn, ops, tg).map(|_| false),

        // --- control flow: ends the block ---
        Jmp => {
            let target = branch_target(insn, ops, tg)?;
            ops.push(IrOp::Jump { target });
            Ok(true)
        }
        Call => {
            let target = branch_target(insn, ops, tg)?;
            ops.push(IrOp::Call {
                target,
                return_addr: insn.next_ip(),
            });
            Ok(true)
        }
        Ret => {
            ops.push(IrOp::Ret);
            Ok(true)
        }
        // leave = mov rsp, rbp; pop rbp.
        Leave => {
            let rbp = read_reg(Reg::Rbp, ops, tg);
            let val = tg.fresh();
            ops.push(IrOp::Load {
                dst: val,
                addr: rbp,
                size: 8,
            });
            let new_rsp = tg.fresh();
            ops.push(IrOp::Add {
                dst: new_rsp,
                a: rbp,
                b: Val::Imm(8),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            ops.push(IrOp::WriteReg {
                reg: Reg::Rbp,
                src: Val::Temp(val),
                size: 8,
            });
            ops.push(IrOp::WriteReg {
                reg: Reg::Rsp,
                src: Val::Temp(new_rsp),
                size: 8,
            });
            Ok(false)
        }
        Syscall => {
            ops.push(IrOp::Syscall);
            Ok(true)
        }
        Hlt => {
            ops.push(IrOp::Hlt);
            Ok(true)
        }

        _ => {
            if let Some(cond) = jcc_cond(insn.mnemonic()) {
                ops.push(IrOp::Branch {
                    cond,
                    taken: insn.near_branch_target(),
                    fallthrough: insn.next_ip(),
                });
                return Ok(true);
            }
            if let Some(cond) = setcc_cond(insn.mnemonic()) {
                return lift_setcc(insn, ops, tg, cond).map(|_| false);
            }
            if let Some(cond) = cmovcc_cond(insn.mnemonic()) {
                return lift_cmovcc(insn, ops, tg, cond).map(|_| false);
            }
            Err(unsupported_insn(insn))
        }
    }
}

// --- per-mnemonic helpers ---

#[derive(Copy, Clone)]
enum BinOp {
    Add,
    Adc,
    Sub,
    Sbb,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
    Rcl,
    Rcr,
}

/// The atomic RMW opcode a lock-prefixed ALU op maps to, if any. `adc`/`sbb`
/// (carry-dependent) and the shifts/rotates have no single-op atomic form and
/// fall back to the (non-atomic) load/op/store path.
fn rmw_of_binop(op: BinOp) -> Option<RmwOp> {
    match op {
        BinOp::Add => Some(RmwOp::Add),
        BinOp::Sub => Some(RmwOp::Sub),
        BinOp::And => Some(RmwOp::And),
        BinOp::Or => Some(RmwOp::Or),
        BinOp::Xor => Some(RmwOp::Xor),
        _ => None,
    }
}

fn mk_binop(op: BinOp, dst: u32, a: Val, b: Val, size: u8, set_flags: FlagMask) -> IrOp {
    // Every `BinOp` variant maps to the identically-named `IrOp` variant with the
    // same `{dst, a, b, size, set_flags}` payload — list the names once, stamp the arms.
    macro_rules! dispatch {
        ($($v:ident),+ $(,)?) => {
            match op {
                $(BinOp::$v => IrOp::$v { dst, a, b, size, set_flags },)+
            }
        };
    }
    dispatch!(Add, Adc, Sub, Sbb, And, Or, Xor, Shl, Shr, Sar, Rol, Ror, Rcl, Rcr)
}

/// Two-operand ALU lift. Handles the register/immediate/memory destination and the
/// read-modify-write case: for a memory destination the effective address is
/// computed ONCE (§7.1) and reused for Load and Store, with the Store emitted
/// before nothing else commits (atomicity, §16 pitfall #0 — flag recompute on
/// retry is idempotent from the same inputs).
fn lift_binop(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: BinOp,
    flags: FlagMask,
    write_back: bool,
) -> Result<(), LiftError> {
    let size = operation_size(insn);

    if insn.op_kind(0) == OpKind::Memory {
        // dst is memory: compute the address once.
        let addr = effective_address(insn, ops, tg)?;

        // `lock`-prefixed ALU RMW → one atomic op + a separate flag recompute
        // (§8.2.3, §11). The flag ALU runs on the atomically-read `old`, so locked
        // ops flag exactly like their plain forms.
        if write_back && insn.has_lock_prefix() {
            if let Some(rop) = rmw_of_binop(op) {
                let b = lower_read(insn, 1, ops, tg)?;
                let old = tg.fresh();
                ops.push(IrOp::AtomicRmw {
                    old,
                    addr,
                    src: b,
                    size,
                    op: rop,
                });
                let res = tg.fresh();
                ops.push(mk_binop(op, res, Val::Temp(old), b, size, flags));
                return Ok(());
            }
        }

        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        let b = lower_read(insn, 1, ops, tg)?;
        let res = tg.fresh();
        ops.push(mk_binop(op, res, a, b, size, flags));
        if write_back {
            ops.push(IrOp::Store {
                addr,
                src: Val::Temp(res),
                size,
                order: MemOrder::None,
            });
        }
        return Ok(());
    }

    let a = lower_read(insn, 0, ops, tg)?;
    let b = lower_read(insn, 1, ops, tg)?;
    let res = tg.fresh();
    ops.push(mk_binop(op, res, a, b, size, flags));
    if write_back {
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(res));
    }
    Ok(())
}

/// `push src` — long-mode default operand size is 8. Store BEFORE committing RSP so
/// a faulting store leaves RSP untouched for the retry (§16 pitfall #0).
fn lift_push(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = push_pop_size(insn);
    let src = lower_read(insn, 0, ops, tg)?;

    let rsp = read_reg(Reg::Rsp, ops, tg);
    let new_rsp = tg.fresh();
    ops.push(IrOp::Sub {
        dst: new_rsp,
        a: rsp,
        b: Val::Imm(size as u64),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    ops.push(IrOp::Store {
        addr: Val::Temp(new_rsp),
        src,
        size,
        order: MemOrder::None,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: 8,
    });
    Ok(())
}

/// `pop dst` — Load BEFORE committing so a faulting load leaves state untouched.
/// `pop rsp` works because the destination write is emitted last and overrides the
/// RSP increment.
fn lift_pop(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = push_pop_size(insn);
    let rsp = read_reg(Reg::Rsp, ops, tg);
    let val = tg.fresh();
    ops.push(IrOp::Load {
        dst: val,
        addr: rsp,
        size,
    });
    let new_rsp = tg.fresh();
    ops.push(IrOp::Add {
        dst: new_rsp,
        a: rsp,
        b: Val::Imm(size as u64),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: 8,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(val));
    Ok(())
}

/// `inc`/`dec`: `op0 ± 1`, preserving CF (`ALL_BUT_CF`). RMW-safe via lift_binop's
/// memory path (the immediate 1 is the second source).
/// Shared skeleton for a single-`r/m`-operand op (`inc`/`dec`/`neg`/`not`, task-172):
/// the three destination paths — `lock` → atomic RMW (+ a flag-recompute on the
/// atomically-read `old` when the op sets flags), plain memory → load/compute/store,
/// register → read/compute/write. The op-specific bits are the atomic `(rmw_op,
/// rmw_src)`, whether it recomputes flags, and `emit`, which pushes the non-atomic
/// compute `res = f(a)` (also reused for the atomic flag-recompute with `a = old`).
fn lift_unary_op0(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    rmw_op: RmwOp,
    rmw_src: Val,
    recompute_flags: bool,
    mut emit: impl FnMut(&mut Vec<IrOp>, crate::ir::Temp, Val, u8),
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        if insn.has_lock_prefix() {
            let old = tg.fresh();
            ops.push(IrOp::AtomicRmw {
                old,
                addr,
                src: rmw_src,
                size,
                op: rmw_op,
            });
            if recompute_flags {
                // Recompute flags on the atomically-read `old`; the result is discarded.
                let res = tg.fresh();
                emit(ops, res, Val::Temp(old), size);
            }
            return Ok(());
        }
        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        let res = tg.fresh();
        emit(ops, res, a, size);
        ops.push(IrOp::Store {
            addr,
            src: Val::Temp(res),
            size,
            order: MemOrder::None,
        });
        return Ok(());
    }
    let a = lower_read(insn, 0, ops, tg)?;
    let res = tg.fresh();
    emit(ops, res, a, size);
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(res));
    Ok(())
}

/// `inc`/`dec`: `op0 ± 1`, flags set but CF preserved (§8.2.3).
fn lift_incdec(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: BinOp,
) -> Result<(), LiftError> {
    let rmw = if matches!(op, BinOp::Add) {
        RmwOp::Add
    } else {
        RmwOp::Sub
    };
    lift_unary_op0(
        insn,
        ops,
        tg,
        rmw,
        Val::Imm(1),
        true,
        |ops, res, a, size| {
            ops.push(mk_binop(
                op,
                res,
                a,
                Val::Imm(1),
                size,
                FlagMask::ALL_BUT_CF,
            ));
        },
    )
}

/// `shld`/`shrd`: double-precision shift. op0 (r/m) is the destination + first
/// source, op1 (r) supplies the fill bits, op2 (imm8 or CL) is the count.
fn lift_double_shift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    left: bool,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let b = lower_read(insn, 1, ops, tg)?;
    let count = lower_read(insn, 2, ops, tg)?;
    let res = tg.fresh();
    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let a = {
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Val::Temp(t)
        };
        ops.push(IrOp::DoubleShift {
            dst: res,
            a,
            b,
            count,
            size,
            left,
            set_flags: FlagMask::SHIFT,
        });
        ops.push(IrOp::Store {
            addr,
            src: Val::Temp(res),
            size,
            order: MemOrder::None,
        });
    } else {
        let a = lower_read(insn, 0, ops, tg)?;
        ops.push(IrOp::DoubleShift {
            dst: res,
            a,
            b,
            count,
            size,
            left,
            set_flags: FlagMask::SHIFT,
        });
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(res));
    }
    Ok(())
}

/// `neg`: `0 - op0`. Flags exactly as `sub` from zero (CF set iff operand ≠ 0).
fn lift_neg(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    // `0 - op0`: reverse-subtract atomic RMW; flags exactly as `sub` from zero.
    lift_unary_op0(
        insn,
        ops,
        tg,
        RmwOp::Rsub,
        Val::Imm(0),
        true,
        |ops, res, a, size| {
            ops.push(IrOp::Sub {
                dst: res,
                a: Val::Imm(0),
                b: a,
                size,
                set_flags: FlagMask::ALL,
            });
        },
    )
}

/// `not`: bitwise complement, NO flags. Lowered as `xor op0, -1` with an empty
/// flag mask (the result is masked to the operand size by the interpreter).
fn lift_not(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    // Bitwise complement = `op0 ^ -1`, a native atomic XOR under `lock`, no flags.
    lift_unary_op0(
        insn,
        ops,
        tg,
        RmwOp::Xor,
        Val::Imm(u64::MAX),
        false,
        |ops, res, a, size| {
            ops.push(IrOp::Xor {
                dst: res,
                a,
                b: Val::Imm(u64::MAX),
                size,
                set_flags: FlagMask::NONE,
            });
        },
    )
}

/// One-operand `mul`/`imul`: `RDX:RAX = RAX * op0`. 8-bit form writes AH (not
/// expressible), so it's rejected; 16/32/64-bit split into RAX (low) and RDX (high).
fn lift_widening_mul(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    signed: bool,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if size < 2 {
        return Err(unsupported_insn(insn));
    }
    let a = read_reg(Reg::Rax, ops, tg);
    let b = lower_read(insn, 0, ops, tg)?;
    let lo = tg.fresh();
    let hi = tg.fresh();
    ops.push(IrOp::Mul {
        lo,
        hi,
        a,
        b,
        size,
        signed,
        set_flags: FlagMask::CF_OF,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rax,
        src: Val::Temp(lo),
        size,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rdx,
        src: Val::Temp(hi),
        size,
    });
    Ok(())
}

/// `imul`: one-operand (`RDX:RAX`), two-operand (`dst *= src`), or three-operand
/// (`dst = src * imm`). The 2/3-operand forms keep only the low half in `dst`;
/// CF/OF still flag overflow of the full signed product.
fn lift_imul(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    match insn.op_count() {
        1 => lift_widening_mul(insn, ops, tg, true),
        2 | 3 => {
            let size = operand_size(insn, 0);
            let (a, b) = if insn.op_count() == 2 {
                (lower_read(insn, 0, ops, tg)?, lower_read(insn, 1, ops, tg)?)
            } else {
                (lower_read(insn, 1, ops, tg)?, lower_read(insn, 2, ops, tg)?)
            };
            let lo = tg.fresh();
            let hi = tg.fresh();
            ops.push(IrOp::Mul {
                lo,
                hi,
                a,
                b,
                size,
                signed: true,
                set_flags: FlagMask::CF_OF,
            });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(lo));
            Ok(())
        }
        _ => Err(unsupported_insn(insn)),
    }
}

/// `div`/`idiv`: `RDX:RAX / op0` → RAX quotient, RDX remainder. May raise `#DE`
/// (zero divisor / overflow) — the `Div` op traps before the register writes, so a
/// retry sees clean state (§16). 8-bit form writes AH, so it's rejected.
fn lift_div(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    signed: bool,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    if size < 2 {
        return Err(unsupported_insn(insn));
    }
    let hi = read_reg(Reg::Rdx, ops, tg);
    let lo = read_reg(Reg::Rax, ops, tg);
    let divisor = lower_read(insn, 0, ops, tg)?;
    let quot = tg.fresh();
    let rem = tg.fresh();
    ops.push(IrOp::Div {
        quot,
        rem,
        hi,
        lo,
        divisor,
        size,
        signed,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rax,
        src: Val::Temp(quot),
        size,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rdx,
        src: Val::Temp(rem),
        size,
    });
    Ok(())
}

/// VEX.128 move: as [`lift_vmov`], but a register destination also clears bits
/// 255:128 of the YMM (task-168.2). A store (mem dest) writes no register.
fn lift_vmov_vex(
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

/// Define a vector/opmask register-index extractor: `Some(index)` when operand
/// `op_idx` is a register of the given class, else `None`. Indices are relative to
/// the class base (XMM0/YMM0/ZMM0/K0), so EVEX high regs (16–31) come through
/// (task-170.3 consolidation of four near-identical extractors).
macro_rules! reg_extractor {
    ($(#[$m:meta])* $name:ident, $pred:ident, $base:ident) => {
        $(#[$m])*
        fn $name(insn: &Instruction, op_idx: u32) -> Option<u8> {
            if insn.op_kind(op_idx) != OpKind::Register {
                return None;
            }
            let r = insn.op_register(op_idx);
            r.$pred().then(|| (r as u32 - Register::$base as u32) as u8)
        }
    };
}

reg_extractor!(
    /// XMM register index (0–31) for an operand, or `None` if it isn't an XMM reg.
    reg_xmm, is_xmm, XMM0
);
reg_extractor!(
    /// YMM register index (0–31) for an operand (task-168.2, AVX-256 path).
    reg_ymm, is_ymm, YMM0
);
reg_extractor!(
    /// ZMM register index (0–31) for an operand (task-168.5, AVX-512 path).
    reg_zmm, is_zmm, ZMM0
);
reg_extractor!(
    /// Opmask register index (k0–k7) for an operand (task-168.5).
    reg_kmask, is_k, K0
);

/// A vector operand's `(register index, byte width)` — XMM=16, YMM=32, ZMM=64.
fn vec_operand(insn: &Instruction, op_idx: u32) -> Option<(u8, u16)> {
    if let Some(z) = reg_zmm(insn, op_idx) {
        Some((z, 64))
    } else if let Some(y) = reg_ymm(insn, op_idx) {
        Some((y, 32))
    } else {
        reg_xmm(insn, op_idx).map(|x| (x, 16))
    }
}

/// Vector register index for an XMM *or* YMM operand (they share the 0–15 file).
fn reg_vec(insn: &Instruction, op_idx: u32) -> Option<u8> {
    reg_xmm(insn, op_idx).or_else(|| reg_ymm(insn, op_idx))
}

/// True if an EVEX instruction carries a write-mask (k1–k7) or zeroing. Such forms
/// need per-element predication we don't yet lift — callers reject them for now
/// (task-168.5, unmasked-first).
fn evex_is_masked(insn: &Instruction) -> bool {
    insn.op_mask() != Register::None || insn.zeroing_masking()
}

/// The EVEX write-mask register index (k1–k7), or `None` for unmasked (k0/none).
fn evex_writemask(insn: &Instruction) -> Option<u8> {
    let r = insn.op_mask();
    if r == Register::None || r == Register::K0 {
        None
    } else {
        Some((r as u32 - Register::K0 as u32) as u8)
    }
}

/// `vpbroadcast{b,w,d,q}` (task-168.3): replicate the low `elem`-byte element of the
/// XMM (or memory) source across the XMM/YMM destination.
fn lift_broadcast(
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

/// VEX packed shift-by-immediate (task-168.3), 3-operand `dst = a << imm` etc.,
/// dispatching on width. VEX.128 clears the dest's upper 128 bits.
fn lift_vpacked_shift_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    lane: u8,
    right: bool,
    arith: bool,
) -> Result<(), LiftError> {
    let d = reg_vec(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_vec(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    if !is_immediate(insn.op_kind(2)) {
        return Err(unsupported_insn(insn)); // variable (register) shift count deferred
    }
    let imm = insn.immediate(2) as u8;
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

/// VEX bitwise logic dispatching on width: a YMM destination routes to the 256-bit
/// `VLogic256`/`VLogic256M` (task-168.2), else the VEX.128 path (task-168.1).
fn lift_vlogic_avx(
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
fn lift_vpacked_bin_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
    op: PackedBinOp,
) -> Result<(), LiftError> {
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
fn lift_vmov_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    // EVEX write-masked reg-reg move `v{k}{z}, v` (task-170.1): blend src into dst
    // under the opmask at `elem` granularity. Register forms only — masked memory
    // load/store (with fault suppression on masked-off lanes) is deferred.
    if evex_is_masked(insn) {
        if let (Some((dst, bytes)), Some((src, _))) = (vec_operand(insn, 0), vec_operand(insn, 1)) {
            if let Some(k) = evex_writemask(insn) {
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
        }
        return Err(unsupported_insn(insn));
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
fn lift_vmov(
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
fn lift_vlogic(
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
fn lift_vpacked_bin(
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
fn lift_vlogic_vex(
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
fn lift_vpacked_bin_vex(
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
fn lift_evex_packed_bin_128(
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

/// SSE4.1 `pmovzx`/`pmovsx` (task-168.5.4): extend `16/to` low `from`-byte elements to
/// `to` bytes each into `dst`. Source is a register (its low bytes) or memory.
fn lift_pmovx(
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

/// EVEX bitwise logic `vpxor{d,q}` / `vpand{d,q}` / `vpor{d,q}` / `vpandn{d,q}`
/// (task-168.5.2). Width-generic (128/256/512) via [`IrOp::VLogicWide`]; the `d`/`q`
/// suffix only picks the mask granularity, irrelevant unmasked. Register src2 only;
/// masked forms are deferred (they belong with the masked-EVEX-data-op work, 168.5.5).
fn lift_evex_vlogic(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: VLogicOp,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VLogicWide {
        dst,
        a,
        b,
        op,
        bytes,
    });
    Ok(())
}

/// EVEX `vpternlog{d,q}` (task-168.5.2): 3-input bitwise logic via an 8-bit truth table.
/// `dst` is both the first source and the destination; `src3` register only (memory
/// deferred); masked forms deferred.
fn lift_vpternlog(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (b, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let (c, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(3) as u8;
    ops.push(IrOp::VPTernlog {
        dst,
        b,
        c,
        imm,
        bytes,
    });
    Ok(())
}

/// `kmov{b,w,d,q}` between opmask, GPR, and memory (task-168.5). `width` in bits.
fn lift_kmov(
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
fn lift_kortest(insn: &Instruction, ops: &mut Vec<IrOp>, width: u8) -> Result<(), LiftError> {
    let a = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VKOrTest { a, b, width });
    Ok(())
}

/// EVEX `vpcmp{,u}{b,w,d,q}` → opmask (task-168.5). `dst = k`, `src1 = op1` (vvvv),
/// `src2 = op2`, predicate = imm8. Register src2 only; memory + write-masked forms
/// deferred.
fn lift_vpcmp(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    elem: u8,
    signed: bool,
) -> Result<(), LiftError> {
    let k = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, width) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let pred = insn.immediate(3) as u8;
    // EVEX write-mask k1–k7 (k0 = unmasked); vpcmp uses it as a compare predicate.
    let writemask = evex_writemask(insn);
    ops.push(IrOp::VPCmpToMask {
        k,
        a,
        b,
        elem,
        width,
        pred,
        signed,
        writemask,
    });
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
fn lift_vpcmp_fixed_or_packed(
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
    let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPCmpToMask {
        k,
        a,
        b,
        elem,
        width,
        pred,
        signed,
        writemask: evex_writemask(insn),
    });
    Ok(())
}

/// Packed shift by immediate `dst = dst << imm` / `>> imm` per lane; a right shift
/// is arithmetic when `arith` (psra*). The register-count form (variable shift) is
/// deferred.
fn lift_vpacked_shift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    lane: u8,
    right: bool,
    arith: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    if !is_immediate(insn.op_kind(1)) {
        return Err(unsupported_insn(insn));
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
fn lift_byteshift(insn: &Instruction, ops: &mut Vec<IrOp>, right: bool) -> Result<(), LiftError> {
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

/// A string op with its repeat prefix. movs/stos/lods take `rep`; scas/cmps take
/// `repe`/`repne` (both share the F3/F2 prefix bytes with the instruction kind).
fn lift_string(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: StrOp,
    elem: u8,
) -> Result<bool, LiftError> {
    let f3 = insn.has_rep_prefix() || insn.has_repe_prefix();
    let f2 = insn.has_repne_prefix();
    let rep = match op {
        StrOp::Scas | StrOp::Cmps => {
            if f2 {
                RepKind::Repne
            } else if f3 {
                RepKind::Repe
            } else {
                RepKind::None
            }
        }
        _ => {
            if f3 {
                RepKind::Rep
            } else {
                RepKind::None
            }
        }
    };
    ops.push(IrOp::RepString { op, elem, rep });
    Ok(false)
}

/// `pshufd`: permute the four 32-bit lanes by imm8 (register source only).
fn lift_pshufd(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
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

/// `shufps`/`shufpd`: interleave two 32-bit (resp. 64-bit) lanes from `dst` with
/// two from `src`. `shufpd`'s 2-bit imm is expanded to the `shufps` selector so
/// one IR op (`VShufps`) covers both.
fn lift_shufps(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
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

/// `pshuflw` (`high`=false) / `pshufhw` (`high`=true): word permute of one 64-bit
/// half. Register source only.
fn lift_pshufw(insn: &Instruction, ops: &mut Vec<IrOp>, high: bool) -> Result<(), LiftError> {
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
fn lift_vunpack(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    lane: u8,
    high: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VUnpackLow {
        dst: d,
        a: d,
        b,
        lane,
        high,
    });
    Ok(())
}

/// `packuswb`: pack dst+src 16-bit lanes to unsigned-saturated bytes.
fn lift_packuswb(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPackUsWB { dst: d, a: d, b });
    Ok(())
}

/// `pinsrw`: insert the low 16 bits of a GPR/memory source into a word lane.
fn lift_pinsrw(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
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
fn lift_pinsr(
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
fn lift_move_half(
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
fn lift_half_mem(
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

/// `pextrw dst_gpr, xmm, imm8`: extract a word lane, zero-extended into the gpr.
fn lift_pextrw(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
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
fn lift_pextr(
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
fn read_scalar_float(
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
fn lift_scalar_fmove(
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

/// Scalar/packed float arithmetic `dst = op(dst, src)` (register or memory source).
fn lift_float_bin(
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

/// `ucomis*`/`comis*`: compare the low lanes and set the arithmetic flags.
fn lift_float_cmp(
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
fn lift_float_cmp_mask(
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

/// `cvtsi2s*`: signed integer (gpr/mem) → float in the destination's low lane.
fn lift_cvt_from_int(
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
    });
    Ok(())
}

/// `cvt(t)s*2si`: float (xmm/mem) → signed integer in the destination GPR.
fn lift_cvt_to_int(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
    trunc: bool,
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
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `sqrts*`/`sqrtp*`: scalar (low lane, upper preserved) or packed square root.
/// Register source (memory source deferred).
fn lift_float_unary(
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
        src: s,
        op,
        prec,
        scalar,
    });
    Ok(())
}

/// `cvtss2sd`/`cvtsd2ss`: convert the low-lane float between precisions.
fn lift_cvt_float(
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

/// `xadd dst, src`: `tmp = dst + src; dst = tmp; src = old_dst`, flags as `add`.
/// A memory destination is atomic (typically `lock`-prefixed, §8.2.3); the source
/// register receives the prior memory value.
fn lift_xadd(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let src = lower_read(insn, 1, ops, tg)?; // source register value

    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let old = tg.fresh();
        ops.push(IrOp::AtomicRmw {
            old,
            addr,
            src,
            size,
            op: RmwOp::Add,
        });
        // flags = add(old, src)
        let res = tg.fresh();
        ops.push(mk_binop(
            BinOp::Add,
            res,
            Val::Temp(old),
            src,
            size,
            FlagMask::ALL,
        ));
        // source register <- old memory value
        let dst1 = lower_write_target(insn, 1, ops, tg)?;
        emit_write(ops, tg, dst1, Val::Temp(old));
        return Ok(());
    }

    // Register destination (non-atomic): dst = dst + src; src = old dst.
    let dst_val = lower_read(insn, 0, ops, tg)?;
    let res = tg.fresh();
    ops.push(mk_binop(BinOp::Add, res, dst_val, src, size, FlagMask::ALL));
    let dst1 = lower_write_target(insn, 1, ops, tg)?;
    emit_write(ops, tg, dst1, dst_val);
    let dst0 = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst0, Val::Temp(res));
    Ok(())
}

/// `cmpxchg dst, src`: compare the accumulator (AL/AX/EAX/RAX) with `dst`; if
/// equal, `dst = src` and ZF=1, else the accumulator takes `dst` and ZF=0. Flags
/// are those of `cmp acc, dst`. A memory destination is atomic (`lock cmpxchg`,
/// §8.2.3) via a single CAS. The register-destination form is deferred (rare, and
/// not a synchronization primitive).
fn lift_cmpxchg(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    if insn.op_kind(0) != OpKind::Memory {
        return Err(unsupported_insn(insn));
    }
    let size = operand_size(insn, 0);
    let addr = effective_address(insn, ops, tg)?;
    let src = lower_read(insn, 1, ops, tg)?;
    // Accumulator, masked to the operand width, is the expected value.
    let acc = read_reg(Reg::Rax, ops, tg);
    let exp = tg.fresh();
    ops.push(IrOp::And {
        dst: exp,
        a: acc,
        b: Val::Imm(size_mask(size)),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    let old = tg.fresh();
    ops.push(IrOp::AtomicCas {
        old,
        addr,
        expected: Val::Temp(exp),
        src,
        size,
    });
    // Flags = cmp(acc, old).
    let res = tg.fresh();
    ops.push(IrOp::Sub {
        dst: res,
        a: Val::Temp(exp),
        b: Val::Temp(old),
        size,
        set_flags: FlagMask::ALL,
    });
    // Accumulator <- old (a no-op on success, the memory value on failure).
    ops.push(IrOp::WriteReg {
        reg: Reg::Rax,
        src: Val::Temp(old),
        size,
    });
    Ok(())
}

/// The ST(i) index referenced by an x87 instruction: the highest ST register
/// among its operands (ST0 is index 0, so a non-zero partner wins). Defaults to 1
/// for the implicit-`st1` forms (`faddp`, `fxch`).
fn st_index(insn: &Instruction) -> u8 {
    let mut idx = None;
    for i in 0..insn.op_count() {
        let r = insn.op_register(i);
        if r >= Register::ST0 && r <= Register::ST7 {
            let n = (r as u32 - Register::ST0 as u32) as u8;
            idx = Some(idx.map_or(n, |c: u8| c.max(n)));
        }
    }
    idx.unwrap_or(1)
}

/// For a register-form x87 arithmetic/store instruction, is the destination ST(0)?
/// (`fsub st(0), st(i)` vs `fsub st(i), st(0)` — op0 is the destination.) Chooses
/// between the `*Sti` (ST0-dest) and `*ToSti` (ST(i)-dest) IR kinds.
fn dst_is_st0(insn: &Instruction) -> bool {
    insn.op0_register() == Register::ST0
}

/// Lift one x87 FPU instruction to an `X87` IR op (§14). Memory operands are
/// reduced to an effective address; register forms carry ST(i) in `sti`.
fn lift_x87(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    use crate::x87::FpuKind as K;
    use Mnemonic::*;

    let mem = (0..insn.op_count()).any(|i| insn.op_kind(i) == OpKind::Memory);
    let msz = insn.memory_size().size();
    let sti = st_index(insn);

    // Emit an X87 op with a freshly computed address (memory forms) or a dummy.
    let emit = |kind: K, ops: &mut Vec<IrOp>, tg: &mut TempGen| -> Result<(), LiftError> {
        let addr = if mem {
            effective_address(insn, ops, tg)?
        } else {
            Val::Imm(0)
        };
        ops.push(IrOp::X87 { kind, addr, sti });
        Ok(())
    };

    match insn.mnemonic() {
        Fld => {
            let k = if mem {
                match msz {
                    4 => K::FldF32,
                    10 => K::FldF80,
                    _ => K::FldF64,
                }
            } else {
                K::FldSti
            };
            emit(k, ops, tg)?;
        }
        Fild => {
            let k = match msz {
                2 => K::FildI16,
                8 => K::FildI64,
                _ => K::FildI32,
            };
            emit(k, ops, tg)?;
        }
        Fst => emit(
            if !mem {
                K::FstSti
            } else if msz == 4 {
                K::FstF32
            } else {
                K::FstF64
            },
            ops,
            tg,
        )?,
        Fstp => {
            let k = if !mem {
                K::FstpSti
            } else {
                match msz {
                    4 => K::FstpF32,
                    10 => K::FstpF80,
                    _ => K::FstpF64,
                }
            };
            emit(k, ops, tg)?;
        }
        Fistp => emit(
            match msz {
                2 => K::FistpI16,
                8 => K::FistpI64,
                _ => K::FistpI32,
            },
            ops,
            tg,
        )?,
        Fadd => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FaddSti
                } else {
                    K::FaddToSti
                }
            } else if msz == 4 {
                K::FaddMemF32
            } else {
                K::FaddMemF64
            },
            ops,
            tg,
        )?,
        Faddp => emit(K::FaddP, ops, tg)?,
        Fsub => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FsubSti
                } else {
                    K::FsubToSti
                }
            } else if msz == 4 {
                K::FsubMemF32
            } else {
                K::FsubMemF64
            },
            ops,
            tg,
        )?,
        Fsubp => emit(K::FsubP, ops, tg)?,
        Fsubr => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FsubrSti
                } else {
                    K::FsubrToSti
                }
            } else if msz == 4 {
                K::FsubrMemF32
            } else {
                K::FsubrMemF64
            },
            ops,
            tg,
        )?,
        Fsubrp => emit(K::FsubrP, ops, tg)?,
        Fmul => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FmulSti
                } else {
                    K::FmulToSti
                }
            } else if msz == 4 {
                K::FmulMemF32
            } else {
                K::FmulMemF64
            },
            ops,
            tg,
        )?,
        Fmulp => emit(K::FmulP, ops, tg)?,
        Fdiv => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FdivSti
                } else {
                    K::FdivToSti
                }
            } else if msz == 4 {
                K::FdivMemF32
            } else {
                K::FdivMemF64
            },
            ops,
            tg,
        )?,
        Fdivp => emit(K::FdivP, ops, tg)?,
        Fdivr => emit(
            if !mem {
                if dst_is_st0(insn) {
                    K::FdivrSti
                } else {
                    K::FdivrToSti
                }
            } else if msz == 4 {
                K::FdivrMemF32
            } else {
                K::FdivrMemF64
            },
            ops,
            tg,
        )?,
        Fdivrp => emit(K::FdivrP, ops, tg)?,
        Fld1 => emit(K::Fld1, ops, tg)?,
        Fldz => emit(K::Fldz, ops, tg)?,
        Fabs => emit(K::Fabs, ops, tg)?,
        Fchs => emit(K::Fchs, ops, tg)?,
        Fxch => emit(K::Fxch, ops, tg)?,
        Fucomi => emit(K::Fucomi, ops, tg)?,
        Fucomip => emit(K::Fucomip, ops, tg)?,
        Fcomi => emit(K::Fcomi, ops, tg)?,
        Fcomip => emit(K::Fcomip, ops, tg)?,
        Fldcw => emit(K::Fldcw, ops, tg)?,
        Fnstcw => emit(K::Fnstcw, ops, tg)?,
        Fnstsw => ops.push(IrOp::X87 {
            kind: K::Fnstsw,
            addr: Val::Imm(0),
            sti: 0,
        }),
        Fprem => emit(K::Fprem, ops, tg)?,
        _ => return Err(unsupported_insn(insn)),
    }
    Ok(())
}

/// `bsf`/`bsr`: bit-scan the source into the destination register, setting ZF.
fn lift_bitscan(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: crate::ir::BitScanOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let src = lower_read(insn, 1, ops, tg)?;
    let old = lower_read(insn, 0, ops, tg)?; // preserved when src == 0 (bsf/bsr)
    let t = tg.fresh();
    ops.push(IrOp::BitScan {
        dst: t,
        src,
        old,
        size,
        op,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `bt`/`bts`/`btr`/`btc`: CF ← the addressed bit; the set/reset/complement forms
/// also write the modified operand back.
///
/// The bit index is masked modulo the operand width — *except* a **register** index
/// against a **memory** operand, which x86 treats as a signed bit-string offset:
/// the addressed byte is `base + (index >> 3)` (arithmetic shift, so a negative
/// index reaches below the base) and the bit within it is `index & 7`. An immediate
/// index is always masked to the operand width (Intel SDM), so its memory form keeps
/// the plain operand-width load/store.
fn lift_bt(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: BtOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let bit = lower_read(insn, 1, ops, tg)?;

    if insn.op_kind(0) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;

        // Register bit index → bit-string addressing at byte granularity; immediate
        // index → masked to the operand width, a plain operand-width access.
        let (ea, esize) = if insn.op_kind(1) == OpKind::Register {
            let idx_size = operand_size(insn, 1);
            // Sign-extend the index to 64 bits, then arithmetic-shift right by 3 to
            // get the (possibly negative) byte displacement from the base address.
            let byte_off = {
                let sext = tg.fresh();
                ops.push(IrOp::Sext {
                    dst: sext,
                    a: bit,
                    from: idx_size,
                });
                let off = tg.fresh();
                ops.push(IrOp::Sar {
                    dst: off,
                    a: Val::Temp(sext),
                    b: Val::Imm(3),
                    size: 8,
                    set_flags: FlagMask::NONE,
                });
                Val::Temp(off)
            };
            // size:1 → the bit index is masked to `& 7` (the bit within the byte).
            (add_addr(addr, byte_off, ops, tg), 1u8)
        } else {
            (addr, size)
        };
        emit_mem_bt(ops, tg, ea, esize, bit, op, insn.has_lock_prefix());
        return Ok(());
    }

    let a = lower_read(insn, 0, ops, tg)?;
    let result = tg.fresh();
    ops.push(IrOp::Bt {
        result,
        a,
        bit,
        size,
        op,
    });
    if !matches!(op, BtOp::Test) {
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(result));
    }
    Ok(())
}

/// Emit the memory-side of `bt`/`bts`/`btr`/`btc` at address `ea`, width `esize`,
/// bit index `bit`, setting CF ← the addressed bit. A **`lock`-prefixed** set/reset/
/// complement compiles to a real atomic RMW (a concurrent `lock bts` on a shared
/// bitmap must not tear the read-modify-write); everything else keeps the plain
/// load-modify-store. `bt` (test) never writes, so its single load is already atomic.
fn emit_mem_bt(
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    ea: Val,
    esize: u8,
    bit: Val,
    op: BtOp,
    locked: bool,
) {
    if locked && !matches!(op, BtOp::Test) {
        // mask = 1 << (bit & (esize*8 - 1)) — the single bit within the accessed unit.
        let width_bits = esize as u64 * 8;
        let shift = {
            let t = tg.fresh();
            ops.push(IrOp::And {
                dst: t,
                a: bit,
                b: Val::Imm(width_bits - 1),
                size: esize,
                set_flags: FlagMask::NONE,
            });
            Val::Temp(t)
        };
        let mask = {
            let t = tg.fresh();
            ops.push(IrOp::Shl {
                dst: t,
                a: Val::Imm(1),
                b: shift,
                size: esize,
                set_flags: FlagMask::NONE,
            });
            Val::Temp(t)
        };
        // set → OR mask; complement → XOR mask; reset → AND ~mask (mask XOR all-ones).
        let (rmw_op, src) = match op {
            BtOp::Set => (RmwOp::Or, mask),
            BtOp::Complement => (RmwOp::Xor, mask),
            BtOp::Reset => {
                let inv = tg.fresh();
                ops.push(IrOp::Xor {
                    dst: inv,
                    a: mask,
                    b: Val::Imm(size_mask(esize)),
                    size: esize,
                    set_flags: FlagMask::NONE,
                });
                (RmwOp::And, Val::Temp(inv))
            }
            BtOp::Test => unreachable!(),
        };
        let old = tg.fresh();
        ops.push(IrOp::AtomicRmw {
            old,
            addr: ea,
            src,
            size: esize,
            op: rmw_op,
        });
        // CF ← the pre-modification bit. `Bt` with `Test` sets CF from `old` and writes
        // nothing; `bit` is masked to the width internally, matching the RMW's `shift`.
        let cf = tg.fresh();
        ops.push(IrOp::Bt {
            result: cf,
            a: Val::Temp(old),
            bit,
            size: esize,
            op: BtOp::Test,
        });
        return;
    }

    let a = {
        let t = tg.fresh();
        ops.push(IrOp::Load {
            dst: t,
            addr: ea,
            size: esize,
        });
        Val::Temp(t)
    };
    let result = tg.fresh();
    ops.push(IrOp::Bt {
        result,
        a,
        bit,
        size: esize,
        op,
    });
    if !matches!(op, BtOp::Test) {
        ops.push(IrOp::Store {
            addr: ea,
            src: Val::Temp(result),
            size: esize,
            order: MemOrder::None,
        });
    }
}

/// `bswap`: reverse the byte order of a 32/64-bit register. No flags.
fn lift_bswap(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let a = lower_read(insn, 0, ops, tg)?;
    let t = tg.fresh();
    ops.push(IrOp::Bswap { dst: t, a, size });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// BMI1/BMI2 single-dst bit op (task-168.5.3): `dst(op0) = op(op1, op2)`. The unary
/// bls* forms have only two operands, so `b` defaults to 0. Reuses `IrOp::Bmi` +
/// `BmiOp` — one seam for the whole family.
fn lift_bmi(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: crate::ir::BmiOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let a = lower_read(insn, 1, ops, tg)?;
    let b = if insn.op_count() >= 3 {
        lower_read(insn, 2, ops, tg)?
    } else {
        Val::Imm(0)
    };
    let t = tg.fresh();
    ops.push(IrOp::Bmi {
        dst: t,
        a,
        b,
        size,
        op,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `mulx dst_hi, dst_lo, src` (BMI2, task-168.5.3): `(hi:lo) = RDX * src`, unsigned,
/// NO flags. Reuses `IrOp::Mul` (which already yields `lo`/`hi` temps). Writing `lo`
/// before `hi` gives the correct `hi` when the two destinations are the same register.
fn lift_mulx(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let rdx = read_reg(Reg::Rdx, ops, tg); // implicit multiplier
    let src = lower_read(insn, 2, ops, tg)?;
    let lo = tg.fresh();
    let hi = tg.fresh();
    ops.push(IrOp::Mul {
        lo,
        hi,
        a: rdx,
        b: src,
        size,
        signed: false,
        set_flags: FlagMask::NONE,
    });
    let dlo = lower_write_target(insn, 1, ops, tg)?;
    emit_write(ops, tg, dlo, Val::Temp(lo));
    let dhi = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dhi, Val::Temp(hi));
    Ok(())
}

/// BMI2 flagless shift/rotate (`shlx`/`shrx`/`sarx`/`rorx`, task-168.5.3): a 3-operand
/// `dst = src <op> count` that sets NO flags — just the existing Shl/Shr/Sar/Ror IR op
/// with `FlagMask::NONE`. `mk` builds the specific op.
fn lift_bmi_shift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mk: impl Fn(crate::ir::Temp, Val, Val, u8, FlagMask) -> IrOp,
) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let a = lower_read(insn, 1, ops, tg)?;
    let b = lower_read(insn, 2, ops, tg)?; // count (reg) or imm8 (rorx)
    let t = tg.fresh();
    ops.push(mk(t, a, b, size, FlagMask::NONE));
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(t));
    Ok(())
}

/// `movbe`: move with byte swap between a register and memory (task-176). Reuses the
/// existing `Bswap` IR op — no new op — around a `Load`/`Store`. `movbe r, m` loads,
/// swaps, writes the register; `movbe m, r` swaps the register, stores. No flags.
fn lift_movbe(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let size = operand_size(insn, 0);
    let addr = effective_address(insn, ops, tg)?;
    if insn.op_kind(1) == OpKind::Memory {
        // movbe reg, [mem]
        let loaded = tg.fresh();
        ops.push(IrOp::Load {
            dst: loaded,
            addr,
            size,
        });
        let swapped = tg.fresh();
        ops.push(IrOp::Bswap {
            dst: swapped,
            a: Val::Temp(loaded),
            size,
        });
        let dst = lower_write_target(insn, 0, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(swapped));
    } else {
        // movbe [mem], reg
        let a = lower_read(insn, 1, ops, tg)?;
        let swapped = tg.fresh();
        ops.push(IrOp::Bswap {
            dst: swapped,
            a,
            size,
        });
        ops.push(IrOp::Store {
            addr,
            src: Val::Temp(swapped),
            size,
            order: MemOrder::None,
        });
    }
    Ok(())
}

/// `xchg dst, src`: swap the two operands. A memory operand makes the swap atomic
/// — on x86 `xchg` with memory is *implicitly* locked (§8.2.3), so it lowers to an
/// atomic exchange (register operand gets the prior memory value). The reg↔reg
/// form is a plain swap. No flags either way.
fn lift_xchg(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    // A memory operand (either position) makes this an atomic exchange; its
    // register partner receives the prior memory value.
    let reg_idx = if insn.op_kind(0) == OpKind::Memory {
        Some(1u32)
    } else if insn.op_kind(1) == OpKind::Memory {
        Some(0u32)
    } else {
        None
    };
    if let Some(reg_idx) = reg_idx {
        let size = operand_size(insn, reg_idx);
        let addr = effective_address(insn, ops, tg)?;
        let reg_val = lower_read(insn, reg_idx, ops, tg)?;
        let old = tg.fresh();
        ops.push(IrOp::AtomicRmw {
            old,
            addr,
            src: reg_val,
            size,
            op: RmwOp::Xchg,
        });
        let dst = lower_write_target(insn, reg_idx, ops, tg)?;
        emit_write(ops, tg, dst, Val::Temp(old));
        return Ok(());
    }

    let a_val = lower_read(insn, 0, ops, tg)?;
    let b_val = lower_read(insn, 1, ops, tg)?;
    let dst0 = lower_write_target(insn, 0, ops, tg)?;
    let dst1 = lower_write_target(insn, 1, ops, tg)?;
    emit_write(ops, tg, dst0, b_val);
    emit_write(ops, tg, dst1, a_val);
    Ok(())
}

/// `movzx`: zero-extend the source (mask to its width), write with the dst width.
fn lift_movzx(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let src_size = operand_size(insn, 1);
    let v = lower_read(insn, 1, ops, tg)?;
    let z = tg.fresh();
    ops.push(IrOp::And {
        dst: z,
        a: v,
        b: Val::Imm(size_mask(src_size)),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(z));
    Ok(())
}

/// `movsx`/`movsxd`: sign-extend the source to 64 bits, write with the dst width.
fn lift_movsx(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Result<(), LiftError> {
    let src_size = operand_size(insn, 1);
    let v = lower_read(insn, 1, ops, tg)?;
    let s = tg.fresh();
    ops.push(IrOp::Sext {
        dst: s,
        a: v,
        from: src_size,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(s));
    Ok(())
}

/// `cdqe`: sign-extend EAX into RAX.
/// `cbw`/`cwde`/`cdqe`: sign-extend the accumulator in place from `from` to `to`
/// bytes (AL→AX, AX→EAX, EAX→RAX). Writing `to=4` zeroes RAX's upper 32 (x86);
/// `to=2` merges into RAX, preserving bits above 16.
fn lift_cbw_family(
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: u8,
    to: u8,
) -> Result<(), LiftError> {
    let rax = read_reg(Reg::Rax, ops, tg);
    let s = tg.fresh();
    ops.push(IrOp::Sext {
        dst: s,
        a: rax,
        from,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rax,
        src: Val::Temp(s),
        size: to,
    });
    Ok(())
}

/// `cqo`: RDX = sign of RAX (arithmetic shift by 63 → all-zero or all-one).
/// `cwd`/`cdq`/`cqo`: fill (D/E/R)DX with the sign of the same-width accumulator
/// (arithmetic shift by width-1). The DX write uses the operand width, so `cdq`
/// zero-extends the upper 32 bits of RDX and `cwd` preserves the upper 48.
fn lift_sign_into_dx(ops: &mut Vec<IrOp>, tg: &mut TempGen, size: u8) -> Result<(), LiftError> {
    let rax = read_reg(Reg::Rax, ops, tg);
    let s = tg.fresh();
    ops.push(IrOp::Sar {
        dst: s,
        a: rax,
        b: Val::Imm(size as u64 * 8 - 1),
        size,
        set_flags: FlagMask::NONE,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rdx,
        src: Val::Temp(s),
        size,
    });
    Ok(())
}

/// `setcc r/m8`: materialize the condition as 0/1 into an 8-bit destination.
fn lift_setcc(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    cond: Cond,
) -> Result<(), LiftError> {
    let c = tg.fresh();
    ops.push(IrOp::GetCond { dst: c, cond });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(c));
    Ok(())
}

/// `cmovcc dst, src`: branchless conditional move. cmov ALWAYS writes the register
/// (so a 32-bit cmov zero-extends even when not taken); the select is
/// `dst ^ ((dst ^ src) & mask)` where `mask` is all-ones iff the condition holds.
fn lift_cmovcc(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    cond: Cond,
) -> Result<(), LiftError> {
    let src = lower_read(insn, 1, ops, tg)?;
    let dst_val = lower_read(insn, 0, ops, tg)?;

    let c = tg.fresh();
    ops.push(IrOp::GetCond { dst: c, cond });
    // mask = 0 - c  → 0x0 or 0xFFFF...FF
    let m = tg.fresh();
    ops.push(IrOp::Sub {
        dst: m,
        a: Val::Imm(0),
        b: Val::Temp(c),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    // diff = dst ^ src
    let diff = tg.fresh();
    ops.push(IrOp::Xor {
        dst: diff,
        a: dst_val,
        b: src,
        size: 8,
        set_flags: FlagMask::NONE,
    });
    // sel = diff & mask
    let sel = tg.fresh();
    ops.push(IrOp::And {
        dst: sel,
        a: Val::Temp(diff),
        b: Val::Temp(m),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    // res = dst ^ sel
    let res = tg.fresh();
    ops.push(IrOp::Xor {
        dst: res,
        a: dst_val,
        b: Val::Temp(sel),
        size: 8,
        set_flags: FlagMask::NONE,
    });
    let dst = lower_write_target(insn, 0, ops, tg)?;
    emit_write(ops, tg, dst, Val::Temp(res));
    Ok(())
}

/// Mask covering the low `size` bytes.
fn size_mask(size: u8) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

// --- operand lowering (§7.1) ---

/// Reduce a SOURCE operand to a `Val` (reads reg / immediate / loads memory).
fn lower_read(
    insn: &Instruction,
    op_idx: u32,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<Val, LiftError> {
    match insn.op_kind(op_idx) {
        OpKind::Register => {
            let r = insn.op_register(op_idx);
            if let Some(parent) = high_byte_parent(r) {
                // Read AH/BH/CH/DH = (parent >> 8) & 0xff.
                let p = read_reg(parent, ops, tg);
                let sh = alu_none(ops, tg, |dst| IrOp::Shr {
                    dst,
                    a: p,
                    b: Val::Imm(8),
                    size: 8,
                    set_flags: FlagMask::NONE,
                });
                return Ok(alu_none(ops, tg, |dst| IrOp::And {
                    dst,
                    a: sh,
                    b: Val::Imm(0xff),
                    size: 8,
                    set_flags: FlagMask::NONE,
                }));
            }
            let reg = iced_to_reg(r).ok_or_else(|| unsupported_insn(insn))?;
            Ok(read_reg(reg, ops, tg))
        }
        OpKind::Memory => {
            let addr = effective_address(insn, ops, tg)?;
            let size = scalar_mem_size(insn, op_idx)?;
            let t = tg.fresh();
            ops.push(IrOp::Load { dst: t, addr, size });
            Ok(Val::Temp(t))
        }
        kind if is_immediate(kind) => Ok(Val::Imm(insn.immediate(op_idx))),
        _ => Err(unsupported_insn(insn)),
    }
}

/// The byte width of a *scalar* memory operand, rejecting widths the generic
/// integer path can't represent (far-pointer `fword`=6, `tbyte`=10, `xmmword`=16 —
/// handled by dedicated x87/SSE arms, not here, or genuinely unsupported). Keeps a
/// malformed guest instruction from lifting to a `Load`/`Store` the JIT can't type
/// (`int_ty`) — it becomes a clean `Exit::UnknownInstruction` instead of a panic.
fn scalar_mem_size(insn: &Instruction, op_idx: u32) -> Result<u8, LiftError> {
    let size = operand_size(insn, op_idx);
    if matches!(size, 1 | 2 | 4 | 8) {
        Ok(size)
    } else {
        Err(unsupported_insn(insn))
    }
}

/// Reduce a DESTINATION operand to a write handle (reg or memory address).
fn lower_write_target(
    insn: &Instruction,
    op_idx: u32,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<WriteTarget, LiftError> {
    match insn.op_kind(op_idx) {
        OpKind::Register => {
            let r = insn.op_register(op_idx);
            if let Some(parent) = high_byte_parent(r) {
                return Ok(WriteTarget::HighByte { parent });
            }
            let reg = iced_to_reg(r).ok_or_else(|| unsupported_insn(insn))?;
            Ok(WriteTarget::Reg {
                reg,
                size: operand_size(insn, op_idx),
            })
        }
        OpKind::Memory => {
            let addr = effective_address(insn, ops, tg)?;
            Ok(WriteTarget::Mem {
                addr,
                size: scalar_mem_size(insn, op_idx)?,
            })
        }
        _ => Err(unsupported_insn(insn)),
    }
}

/// The GPR whose bits 8–15 a high-byte register names, or `None`.
fn high_byte_parent(reg: Register) -> Option<Reg> {
    match reg {
        Register::AH => Some(Reg::Rax),
        Register::BH => Some(Reg::Rbx),
        Register::CH => Some(Reg::Rcx),
        Register::DH => Some(Reg::Rdx),
        _ => None,
    }
}

/// Emit `base + index*scale + disp`, returning a `Val` holding the address.
/// The ONE place an address is computed (§17.5). Uses iced's folded RIP-relative
/// value (next-insn base) and adds FS/GS base when a segment prefix is present.
fn effective_address(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<Val, LiftError> {
    let addr = effective_address_no_segment(insn, ops, tg)?;
    Ok(with_segment(insn, addr, ops, tg))
}

/// The address arithmetic without the segment base — for `lea`, which computes the
/// offset and *ignores* the segment (`lea rax, fs:[rbx]` is `rax = rbx`, not
/// `rbx + fs_base`). Every memory *access* goes through [`effective_address`], which
/// wraps this with [`with_segment`].
fn effective_address_no_segment(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<Val, LiftError> {
    let base = insn.memory_base();
    let index = insn.memory_index();
    let scale = insn.memory_index_scale();
    let disp = insn.memory_displacement64();

    // RIP-relative: iced already folded RIP+disp into an absolute address. Under a
    // 32-bit address-size override iced reports `EIP` (not `RIP`); truncate the folded
    // value to 32 bits, the same wrap the register-form mask below applies.
    if base == Register::RIP {
        return Ok(Val::Imm(disp));
    }
    if base == Register::EIP {
        return Ok(Val::Imm(disp & 0xFFFF_FFFF));
    }

    let mut acc: Option<Val> = None;

    if base != Register::None {
        let reg = iced_to_reg(base).ok_or_else(|| unsupported_insn(insn))?;
        acc = Some(read_reg(reg, ops, tg));
    }

    if index != Register::None {
        let reg = iced_to_reg(index).ok_or_else(|| unsupported_insn(insn))?;
        let idx = read_reg(reg, ops, tg);
        let scaled = if scale <= 1 {
            idx
        } else {
            // scale ∈ {2,4,8} → shift by {1,2,3}. No flags.
            let shift = scale.trailing_zeros() as u64;
            let t = tg.fresh();
            ops.push(IrOp::Shl {
                dst: t,
                a: idx,
                b: Val::Imm(shift),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            Val::Temp(t)
        };
        acc = Some(match acc {
            None => scaled,
            Some(a) => add_addr(a, scaled, ops, tg),
        });
    }

    let addr = match acc {
        None => Val::Imm(disp),
        Some(a) if disp == 0 => a,
        Some(a) => add_addr(a, Val::Imm(disp), ops, tg),
    };

    // 32-bit address-size override (0x67): the effective address is truncated to 32
    // bits. iced encodes the 32-bit form with 32-bit base/index registers (EBX, not
    // RBX), so a 4-byte-wide base or index flags it — mask the computed offset.
    if base.size() == 4 || index.size() == 4 {
        return Ok(match addr {
            Val::Imm(v) => Val::Imm(v & 0xFFFF_FFFF),
            a => {
                let t = tg.fresh();
                ops.push(IrOp::And {
                    dst: t,
                    a,
                    b: Val::Imm(0xFFFF_FFFF),
                    size: 8,
                    set_flags: FlagMask::NONE,
                });
                Val::Temp(t)
            }
        });
    }

    Ok(addr)
}

/// Add the FS/GS segment base if the instruction carries that prefix (TLS, §7.1).
fn with_segment(insn: &Instruction, addr: Val, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val {
    let seg = match insn.segment_prefix() {
        Register::FS => Reg::FsBase,
        Register::GS => Reg::GsBase,
        _ => return addr,
    };
    let base = read_reg(seg, ops, tg);
    add_addr(addr, base, ops, tg)
}

/// Emit a non-flag-setting 64-bit address addition.
fn add_addr(a: Val, b: Val, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val {
    let t = tg.fresh();
    ops.push(IrOp::Add {
        dst: t,
        a,
        b,
        size: 8,
        set_flags: FlagMask::NONE,
    });
    Val::Temp(t)
}

/// Emit a `ReadReg` and return the temp holding the value.
fn read_reg(reg: Reg, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val {
    let t = tg.fresh();
    ops.push(IrOp::ReadReg { dst: t, reg });
    Val::Temp(t)
}

fn emit_write(ops: &mut Vec<IrOp>, tg: &mut TempGen, target: WriteTarget, value: Val) {
    match target {
        WriteTarget::Reg { reg, size } => ops.push(IrOp::WriteReg {
            reg,
            src: value,
            size,
        }),
        WriteTarget::Mem { addr, size } => ops.push(IrOp::Store {
            addr,
            src: value,
            size,
            order: MemOrder::None,
        }),
        // AH/BH/CH/DH: parent = (parent & ~0xff00) | ((value & 0xff) << 8).
        WriteTarget::HighByte { parent } => {
            let cur = read_reg(parent, ops, tg);
            let clear = alu_none(ops, tg, |dst| IrOp::And {
                dst,
                a: cur,
                b: Val::Imm(!0xff00u64),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            let byte = alu_none(ops, tg, |dst| IrOp::And {
                dst,
                a: value,
                b: Val::Imm(0xff),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            let shifted = alu_none(ops, tg, |dst| IrOp::Shl {
                dst,
                a: byte,
                b: Val::Imm(8),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            let merged = alu_none(ops, tg, |dst| IrOp::Or {
                dst,
                a: clear,
                b: shifted,
                size: 8,
                set_flags: FlagMask::NONE,
            });
            ops.push(IrOp::WriteReg {
                reg: parent,
                src: merged,
                size: 8,
            });
        }
    }
}

/// Emit a flag-free op producing a fresh temp, returning it as a `Val`.
fn alu_none(ops: &mut Vec<IrOp>, tg: &mut TempGen, mk: impl FnOnce(u32) -> IrOp) -> Val {
    let t = tg.fresh();
    ops.push(mk(t));
    Val::Temp(t)
}

/// Target `Val` for a jmp/call: an immediate for a near (rel) branch, otherwise the
/// value of the indirect register/memory operand.
fn branch_target(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<Val, LiftError> {
    match insn.op_kind(0) {
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
            Ok(Val::Imm(insn.near_branch_target()))
        }
        _ => lower_read(insn, 0, ops, tg),
    }
}

// --- small helpers ---

/// Map an iced register (any width GPR) to our `Reg`. `None` for high-byte
/// registers (AH/BH/CH/DH, which write bits 8–15 and can't be expressed by the
/// 64-bit `Reg` enum) and non-GPRs — the caller turns `None` into `Unsupported`
/// rather than mis-lowering to the low byte.
fn iced_to_reg(reg: Register) -> Option<Reg> {
    if matches!(
        reg,
        Register::AH | Register::BH | Register::CH | Register::DH
    ) {
        return None;
    }
    iced_gpr_index(reg).map(Reg::from_gpr_index)
}

fn is_immediate(kind: OpKind) -> bool {
    matches!(
        kind,
        OpKind::Immediate8
            | OpKind::Immediate8_2nd
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate64
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64
    )
}

/// Size in bytes of one operand (register width or memory-access width).
fn operand_size(insn: &Instruction, op_idx: u32) -> u8 {
    match insn.op_kind(op_idx) {
        OpKind::Register => insn.op_register(op_idx).size() as u8,
        OpKind::Memory => insn.memory_size().size() as u8,
        _ => 0,
    }
}

/// Width of a binary operation = size of operand 0 (the destination), falling back
/// to operand 1 for the rare all-immediate/implicit form.
fn operation_size(insn: &Instruction) -> u8 {
    let s = operand_size(insn, 0);
    if s != 0 {
        s
    } else {
        operand_size(insn, 1)
    }
}

/// push/pop transfer size — long-mode default is 8; a 16-bit operand overrides.
fn push_pop_size(insn: &Instruction) -> u8 {
    let s = operand_size(insn, 0);
    if s == 0 {
        8
    } else {
        s
    }
}

// The Jcc / SETcc / CMOVcc families share one condition set in one order; each row below
// is a condition and its three mnemonics, so the `Cond` is named once for all three tables.
macro_rules! cond_tables {
    ($( $cond:ident : $jcc:ident, $setcc:ident, $cmovcc:ident ; )+) => {
        fn jcc_cond(m: Mnemonic) -> Option<Cond> {
            use Mnemonic::*;
            Some(match m { $($jcc => Cond::$cond,)+ _ => return None })
        }
        fn setcc_cond(m: Mnemonic) -> Option<Cond> {
            use Mnemonic::*;
            Some(match m { $($setcc => Cond::$cond,)+ _ => return None })
        }
        fn cmovcc_cond(m: Mnemonic) -> Option<Cond> {
            use Mnemonic::*;
            Some(match m { $($cmovcc => Cond::$cond,)+ _ => return None })
        }
    };
}
cond_tables! {
    Eq:         Je,  Sete,  Cmove;
    Ne:         Jne, Setne, Cmovne;
    Below:      Jb,  Setb,  Cmovb;
    AboveEq:    Jae, Setae, Cmovae;
    BelowEq:    Jbe, Setbe, Cmovbe;
    Above:      Ja,  Seta,  Cmova;
    Less:       Jl,  Setl,  Cmovl;
    GreaterEq:  Jge, Setge, Cmovge;
    LessEq:     Jle, Setle, Cmovle;
    Greater:    Jg,  Setg,  Cmovg;
    Sign:       Js,  Sets,  Cmovs;
    NoSign:     Jns, Setns, Cmovns;
    Overflow:   Jo,  Seto,  Cmovo;
    NoOverflow: Jno, Setno, Cmovno;
    Parity:     Jp,  Setp,  Cmovp;
    NoParity:   Jnp, Setnp, Cmovnp;
}

fn unsupported_insn(insn: &Instruction) -> LiftError {
    LiftError::Unsupported {
        addr: insn.ip(),
        bytes: [0; 15],
        len: insn.len() as u8,
    }
}

/// Fill in the real instruction bytes on an `Unsupported` error (the ~40 lift
/// helpers build it with a zeroed placeholder, since they don't hold the code
/// slice). Called once at the decode loop, which does — so `Exit::UnknownInstruction`
/// reports the actual opcode for compat triage instead of 15 zero bytes.
fn refill_unsupported_bytes(err: LiftError, code: &[u8], block_start: u64) -> LiftError {
    if let LiftError::Unsupported {
        addr,
        mut bytes,
        len,
    } = err
    {
        let off = (addr - block_start) as usize;
        if let Some(slice) = code.get(off..off + len as usize) {
            bytes[..len as usize].copy_from_slice(slice);
        }
        return LiftError::Unsupported { addr, bytes, len };
    }
    err
}
