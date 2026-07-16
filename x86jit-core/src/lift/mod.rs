//! Lift: x86 -> IR (§7).
//!
//! Two levels (§7.1): an operand-lowering layer beneath the per-mnemonic lift.
//! Every operand is reduced to a [`Val`] via `lower_read` / `lower_write_target`
//! before an op is emitted; memory operands expand to effective-address arithmetic
//! (the single `effective_address` helper, §17.5) plus `Load`/`Store`.

use iced_x86::{
    Code, CodeSize, Decoder, DecoderError, DecoderOptions, Instruction, Mnemonic, OpKind, Register,
};

use crate::ir::{
    AesOp, BtOp, Cond, FPrec, FlagMask, FloatBinOp, FloatUnOp, GfniOp, HFloatOp, HIntOp, IrBlock,
    IrOp, IrRegion, MemOrder, PackedBinOp, PackedCvtKind, RegionCaps, RepKind, RmwOp, ShaOp, StrOp,
    Temp, TempGen, VKLogicOp, VLogicOp, Val, VpUnaryOp,
};
use crate::memory::Memory;
use crate::state::{iced_gpr_index, Reg};

mod control;
mod integer;
mod vector;

use control::*;
use integer::*;
use vector::*;

/// Guest decode/lift context (§17.3): the effective operand/address-size default a
/// block of bytes decodes and lifts under. This is a *decode context*, not the
/// architectural mode register — a value threaded from Vm construction through the
/// dispatcher into the decoder, keeping the literal `64` out of `Decoder::new`. It is
/// also block-cache key material (§17.4, `BlockKey`): the same bytes decode
/// differently per mode, so each mode gets its own translation.
///
/// `Compat32` (32-bit protected/compat, flat segments) wires 32-bit control-flow and
/// stack semantics here (task-197.3): EIP truncation on jmp/jcc/call/ret, 4-byte
/// push/pop/call frames (2-byte under 66h), and ESP wrap mod 2^32. Effective-address
/// truncation / 67h addressing is task-197.2; the loader's §17.7 rejection of non-i386
/// ELFs is task-197.4.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum CpuMode {
    Long64,
    Compat32,
}

impl CpuMode {
    pub fn bits(self) -> u32 {
        match self {
            CpuMode::Long64 => 64,
            CpuMode::Compat32 => 32,
        }
    }

    /// `true` when the instruction-pointer and stack-pointer wrap at 2^32 (§16, §17.3):
    /// Compat32 truncates every computed EIP/ESP to 32 bits. Long mode does not.
    fn wraps_32(self) -> bool {
        matches!(self, CpuMode::Compat32)
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
pub fn lift_block(mem: &Memory, start: u64, mode: CpuMode) -> Result<IrBlock, LiftError> {
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

        let terminated = lift_insn(&insn, &mut ops, &mut tg, mode)
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
pub fn lift_one(mem: &Memory, start: u64, mode: CpuMode) -> Result<IrBlock, LiftError> {
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
    lift_insn(&insn, &mut ops, &mut tg, mode)
        .map_err(|e| refill_unsupported_bytes(e, code, start))?;
    elide_dead_flags(&mut ops);

    Ok(IrBlock {
        guest_start: start,
        ops,
        temp_count: tg.count(),
        guest_len: insn.len() as u32,
        icount: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryModel, Prot, RegionKind};

    const BASE: u64 = 0x1000;

    fn mem_with(bytes: &[u8]) -> Memory {
        let mut m = Memory::new(MemoryModel::Flat { size: 0x4000 });
        m.map(BASE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
        m.write_bytes(BASE, bytes).unwrap();
        m
    }

    /// §17.3 seam: decoder bitness is driven purely by the threaded `CpuMode`, never a
    /// hardcoded literal. `48 FF C0 C3` decodes differently per mode — `48` is REX.W in
    /// long mode (`inc rax`, one 3-byte insn) but a full instruction in 32-bit mode
    /// (`dec eax`, one byte). Same bytes, same entry, distinct lifts driven only by the
    /// mode argument — so `lift_block`/`lift_one` really honor the parameter.
    #[test]
    fn lift_bitness_comes_from_mode_argument() {
        let bytes = &[0x48, 0xFF, 0xC0, 0xC3]; // long: inc rax; ret — 32-bit: dec eax; inc eax; ret
        let mem = mem_with(bytes);

        let long = lift_block(&mem, BASE, CpuMode::Long64).expect("lift long");
        let compat = lift_block(&mem, BASE, CpuMode::Compat32).expect("lift compat");
        assert!(
            compat.icount > long.icount,
            "32-bit decode splits the REX.W prefix into more instructions \
             (long={}, compat={})",
            long.icount,
            compat.icount,
        );

        // lift_one honors the mode too: the first instruction's guest length differs
        // (3 bytes `inc rax` in long mode vs 1 byte `dec eax` in 32-bit).
        let one_long = lift_one(&mem, BASE, CpuMode::Long64).expect("one long");
        let one_compat = lift_one(&mem, BASE, CpuMode::Compat32).expect("one compat");
        assert_eq!(one_long.guest_len, 3);
        assert_eq!(one_compat.guest_len, 1);
    }

    /// `int 0x80` (`CD 80`) is the Linux i386 syscall gate: it lifts to `IrOp::Syscall`
    /// (surfaced as `Exit::Syscall`, like `syscall`), while any other `int n` is a
    /// guest-raised software interrupt lifting to a `Trap` to that vector (TASK-197.4).
    #[test]
    fn int_0x80_is_syscall_other_int_is_trap() {
        let syscall_gate = mem_with(&[0xCD, 0x80]); // int 0x80
        let blk = lift_one(&syscall_gate, BASE, CpuMode::Compat32).expect("lift int 0x80");
        assert!(
            matches!(blk.ops.last(), Some(IrOp::Syscall { is_amd64: false })),
            "int 0x80 must lift to Syscall (i386, no RCX/R11 latch), got {:?}",
            blk.ops.last()
        );

        let other = mem_with(&[0xCD, 0x2A]); // int 0x2a
        let blk = lift_one(&other, BASE, CpuMode::Compat32).expect("lift int 0x2a");
        assert!(
            matches!(
                blk.ops.last(),
                Some(IrOp::Trap {
                    vector: 0x2a,
                    advance: 2
                })
            ),
            "int 0x2a must trap to vector 0x2a, got {:?}",
            blk.ops.last()
        );
    }
}

/// The static (`Val::Imm`) successor addresses of a block: an unconditional jump's
/// target, or a conditional branch's two arms. Indirect jumps / call / ret / etc.
/// have no static successors (their edges leave the region).
pub(crate) fn static_succs(block: &IrBlock) -> Vec<u64> {
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
pub fn lift_region(
    mem: &Memory,
    entry: u64,
    caps: RegionCaps,
    mode: CpuMode,
) -> Result<IrRegion, LiftError> {
    use std::collections::HashMap;

    // DFS from the entry, lifting each block once, collecting a post-order.
    fn dfs(
        mem: &Memory,
        addr: u64,
        caps: RegionCaps,
        mode: CpuMode,
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
            if let Ok(b) = lift_block(mem, s, mode) {
                *icount += b.icount;
                blocks.insert(s, b);
                dfs(mem, s, caps, mode, blocks, post, icount);
            }
            // an unliftable successor simply stays an exit edge
        }
        post.push(addr); // finished: post-order
    }

    let first = lift_block(mem, entry, mode)?;
    let mut icount = first.icount;
    let mut blocks = HashMap::from([(entry, first)]);
    let mut post = Vec::new();
    dfs(mem, entry, caps, mode, &mut blocks, &mut post, &mut icount);

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
pub(crate) fn cond_reads(cond: Cond) -> u8 {
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
pub(crate) fn op_reads(op: &IrOp) -> u8 {
    match op {
        IrOp::Branch { cond, .. } | IrOp::GetCond { cond, .. } => cond_reads(*cond),
        IrOp::Adc { .. } | IrOp::Sbb { .. } | IrOp::Rcl { .. } | IrOp::Rcr { .. } => F_CF,
        _ => 0,
    }
}

/// Whether an op is a *count-conditional* flag writer whose count could be 0 (§16).
/// The variable-count shifts and rotates (`shl/shr/sar/rol/ror/rcl/rcr reg,cl`, and
/// `shld/shrd`) write NO flags when the masked count is 0 — the write is only a
/// *possibility*, not a certainty. For dead-flag liveness this is the dangerous case:
/// such an op must NOT be treated as killing a prior flag producer, because on the
/// count==0 path the earlier flags flow straight through and can still be read after it.
///
/// Returns `true` when the flag write may be skipped:
///   * the count is a runtime `Temp` (CL) — it could be 0 at run time, or
///   * the count is an immediate that masks to 0 (e.g. `shl r32, 32`) — a definite no-op.
///
/// Returns `false` for an immediate count that masks to non-zero (a definite write) and
/// for any op that is not a count-conditional shift/rotate.
///
/// The mask (0x3f for 64-bit operands, 0x1f otherwise) is applied before the zero test,
/// matching the interpreter/JIT `shift_mask`, so a CL immediate of 32 on a 32-bit shift
/// (masks to 0) is correctly seen as possibly-skipping its flag write.
fn shift_flags_may_skip(op: &IrOp) -> bool {
    use IrOp::*;
    // `through_carry` marks rcl/rcr, whose count is further reduced modulo (bits+1) before
    // the flag-write decision — so an immediate that masks to non-zero can still reduce to 0
    // on 8/16-bit ops (e.g. `rcl al, 9` → 9 % 9 == 0) and skip the flag write.
    let (count, size, through_carry) = match op {
        Shl { b, size, .. }
        | Shr { b, size, .. }
        | Sar { b, size, .. }
        | Rol { b, size, .. }
        | Ror { b, size, .. } => (b, size, false),
        Rcl { b, size, .. } | Rcr { b, size, .. } => (b, size, true),
        DoubleShift { count, size, .. } => (count, size, false),
        _ => return false,
    };
    let cmask: u64 = if *size == 8 { 0x3f } else { 0x1f };
    match count {
        // Static count: the flag write is skipped iff the effective count is 0.
        Val::Imm(n) => {
            let masked = n & cmask;
            let eff = if through_carry {
                masked % (*size as u64 * 8 + 1) // rcl/rcr reduce modulo (bits+1)
            } else {
                masked
            };
            eff == 0
        }
        // Runtime count (CL): could be 0, so the flag write is only conditional.
        Val::Temp(_) => true,
    }
}

/// The mutable flag-write mask of an op that carries one (the ALU ops).
pub(crate) fn op_set_flags_mut(op: &mut IrOp) -> Option<&mut FlagMask> {
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
pub(crate) fn elide_dead_flags(ops: &mut [IrOp]) {
    if std::env::var_os("X86JIT_NO_FLAG_ELISION").is_some() {
        return; // experiment: keep every ALU op's full flag mask (task-215 bug hunt)
    }
    let mut live: u8 = 0b11_1111; // all flags live-out at the block boundary
    for op in ops.iter_mut().rev() {
        let reads = op_reads(op);
        // A variable-count shift/rotate writes NO flags when the masked count is 0, so
        // it is only a *possible* clobber: a prior flag producer stays live across it
        // (task-224). We still narrow the op's own mask to the live set — on the
        // count!=0 path it is the producer of exactly those flags — but we must NOT
        // clear them from `live` for the ops that precede it.
        let may_skip = shift_flags_may_skip(op);
        if let Some(mask) = op_set_flags_mut(op) {
            mask.0 &= live; // keep only the still-live flags this op writes
                            // Effective *unconditional* writes: 0 when the flag write may be skipped
                            // (runtime or masks-to-0 count), so the written flags remain live upward.
            let kills = if may_skip { 0 } else { mask.0 };
            live = (live & !kills) | reads;
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
pub(crate) use vec_src_dispatch;

/// Lift one instruction; returns `true` if it ends the block (control flow).
pub(crate) fn lift_insn(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mode: CpuMode,
) -> Result<bool, LiftError> {
    use Mnemonic::*;
    match insn.mnemonic() {
        // No architectural effect for our purposes (CET markers, pause hint).
        // CET markers / hints that are no-ops without shadow stacks: endbr, pause,
        // and rdssp (leaves its register — glibc's `xor eax; rdsspq rax; test`
        // then correctly detects "no shadow stack"). Prefetch (`0F 18`, `0F 0D`) is a
        // pure cache hint with no architectural effect (Go's runtime memmove emits it).
        // Wait (0x9B, FWAIT/WAIT) is an x87 sync barrier: with no pending unmasked x87
        // exceptions modeled it is a no-op (Orbis CRT emits it as padding, task-194).
        Nop | Endbr64 | Endbr32 | Pause | Wait | Rdsspd | Rdsspq | Prefetchnta | Prefetcht0
        | Prefetcht1 | Prefetcht2 | Prefetchw | Prefetchwt1 => Ok(false),

        // `movnti` is a non-temporal GPR→mem store; the cache-bypass hint has no
        // architectural effect in our coherent single-buffer model, so it lowers to a
        // plain sized store, identical to `mov [mem], reg` (task-164).
        Mov | Movnti => {
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
        // SAL is the /6 encoding alias of SHL — identical semantics.
        Shl | Sal => lift_binop(insn, ops, tg, BinOp::Shl, FlagMask::SHIFT, true).map(|_| false),
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
        Fld | Fst | Fstp | Fild | Fistp | Fisttp | Fadd | Faddp | Fsub | Fsubp | Fsubr | Fsubrp
        | Fmul | Fmulp | Fdiv | Fdivp | Fdivr | Fdivrp | Fld1 | Fldz | Fabs | Fchs | Fxch
        | Fucomi | Fucomip | Fcomi | Fcomip | Fldcw | Fnstcw | Fnstsw | Fprem | Fsin | Fcos
        | Fptan | Fpatan | F2xm1 | Fyl2x | Fyl2xp1 | Fsincos => {
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
        Stmxcsr | Vstmxcsr => {
            let addr = effective_address(insn, ops, tg)?;
            ops.push(IrOp::Store {
                addr,
                src: Val::Imm(0x1F80),
                size: 4,
                order: MemOrder::None,
            });
            Ok(false)
        }
        Ldmxcsr | Vldmxcsr => Ok(false),
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
        Movsb => lift_string(insn, ops, tg, StrOp::Movs, 1),
        Movsw => lift_string(insn, ops, tg, StrOp::Movs, 2),
        Movsq => lift_string(insn, ops, tg, StrOp::Movs, 8),
        Stosb => lift_string(insn, ops, tg, StrOp::Stos, 1),
        Stosw => lift_string(insn, ops, tg, StrOp::Stos, 2),
        Stosd => lift_string(insn, ops, tg, StrOp::Stos, 4),
        Stosq => lift_string(insn, ops, tg, StrOp::Stos, 8),
        Lodsb => lift_string(insn, ops, tg, StrOp::Lods, 1),
        Lodsw => lift_string(insn, ops, tg, StrOp::Lods, 2),
        Lodsd => lift_string(insn, ops, tg, StrOp::Lods, 4),
        Lodsq => lift_string(insn, ops, tg, StrOp::Lods, 8),
        Scasb => lift_string(insn, ops, tg, StrOp::Scas, 1),
        Scasw => lift_string(insn, ops, tg, StrOp::Scas, 2),
        Scasd => lift_string(insn, ops, tg, StrOp::Scas, 4),
        Scasq => lift_string(insn, ops, tg, StrOp::Scas, 8),
        Cmpsb => lift_string(insn, ops, tg, StrOp::Cmps, 1),
        Cmpsw => lift_string(insn, ops, tg, StrOp::Cmps, 2),
        Cmpsq => lift_string(insn, ops, tg, StrOp::Cmps, 8),
        // Movsd/Cmpsd/Movss... also name SSE scalar moves — route the memory-operand
        // (string) form here, defer the xmm form.
        Movsd if reg_xmm(insn, 0).is_none() && reg_xmm(insn, 1).is_none() => {
            lift_string(insn, ops, tg, StrOp::Movs, 4)
        }
        Cmpsd if reg_xmm(insn, 0).is_none() && reg_xmm(insn, 1).is_none() => {
            lift_string(insn, ops, tg, StrOp::Cmps, 4)
        }
        // xmm form: compare-scalar-double with a predicate imm.
        Cmpsd => lift_float_cmp_mask(insn, ops, tg, FPrec::F64, true).map(|_| false),

        // --- SSE data movement + logic (§3.1 M8) ---
        // Non-temporal 128-bit vector stores (`movntdq`/`movntps`/`movntpd`): the
        // cache-bypass hint is a no-op in our model, so they lower like `movdqu` — a
        // plain 16-byte vector store (task-164).
        // `movntdqa` ([mem] -> xmm, 66 0F38 2A) is the non-temporal aligned *load*; the
        // streaming-read hint is a no-op in our coherent model, so it lowers like `movdqa`
        // (task-246).
        Movdqa | Movdqu | Movaps | Movups | Movapd | Movupd | Movntdq | Movntps | Movntpd
        | Movntdqa => lift_vmov(insn, ops, tg, 16).map(|_| false),
        Movq => lift_vmov(insn, ops, tg, 8).map(|_| false),
        Movd => lift_vmov(insn, ops, tg, 4).map(|_| false),
        Movlhps => lift_move_half(insn, ops, true, false).map(|_| false),
        Movhlps => lift_move_half(insn, ops, false, true).map(|_| false),
        // VEX.128 3-operand forms (task-252) — reg-only, a 64-bit-lane unpack.
        Vmovlhps => lift_vmov_packed_half(insn, ops, false).map(|_| false),
        Vmovhlps => lift_vmov_packed_half(insn, ops, true).map(|_| false),
        Movhps | Movhpd => lift_half_mem(insn, ops, tg, true).map(|_| false),
        Movlps | Movlpd => lift_half_mem(insn, ops, tg, false).map(|_| false),
        // VEX half-vector moves (task-195). The store form `[mem], xmm` is operand-identical
        // to SSE; the 3-operand VEX load form is handled inside `lift_half_mem`.
        Vmovhps | Vmovhpd => lift_vhalf_mem(insn, ops, tg, true).map(|_| false),
        Vmovlps | Vmovlpd => lift_vhalf_mem(insn, ops, tg, false).map(|_| false),
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
        // SSE2 saturating add/sub + rounding average (task-190).
        Paddsb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::AddSatS).map(|_| false),
        Paddsw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::AddSatS).map(|_| false),
        Paddusb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::AddSatU).map(|_| false),
        Paddusw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::AddSatU).map(|_| false),
        Psubsb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::SubSatS).map(|_| false),
        Psubsw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::SubSatS).map(|_| false),
        Psubusb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::SubSatU).map(|_| false),
        Psubusw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::SubSatU).map(|_| false),
        Pavgb => lift_vpacked_bin(insn, ops, tg, 1, PackedBinOp::AvgU).map(|_| false),
        Pavgw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::AvgU).map(|_| false),
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
        // VEX.128 whole-lane byte shift (task-195): 3-operand `dst,a,imm8` + 255:128 clear.
        Vpsrldq => lift_byteshift_avx(insn, ops, true).map(|_| false),
        Vpslldq => lift_byteshift_avx(insn, ops, false).map(|_| false),

        // shuffles / unpacks / pack / insert
        Pshufd => lift_pshufd(insn, ops, tg).map(|_| false),
        Vpshufd => lift_vpshufd(insn, ops, tg).map(|_| false),
        // SSE3 lane-duplicating moves (task-253) — fixed dword shuffles reusing VShuffle32.
        Movsldup => lift_movdup(insn, ops, tg, 0xA0, false).map(|_| false),
        Movshdup => lift_movdup(insn, ops, tg, 0xF5, false).map(|_| false),
        Movddup => lift_movdup(insn, ops, tg, 0x44, true).map(|_| false),
        Vmovsldup => lift_vmovdup(insn, ops, tg, 0xA0, false).map(|_| false),
        Vmovshdup => lift_vmovdup(insn, ops, tg, 0xF5, false).map(|_| false),
        Vmovddup => lift_vmovdup(insn, ops, tg, 0x44, true).map(|_| false),
        Pshuflw => lift_pshufw(insn, ops, false).map(|_| false),
        Pshufhw => lift_pshufw(insn, ops, true).map(|_| false),
        Shufps | Shufpd => lift_shufps(insn, ops).map(|_| false),
        // AVX `vshufps`/`vshufpd` (task-257): the VEX 3-operand shuffle — distinct merge base
        // (vvvv), register or m128 src2, + VEX.128 upper-lane zeroing. Reuses VShufps.
        Vshufps | Vshufpd => lift_vshufps(insn, ops, tg).map(|_| false),
        Vpermilps => lift_vpermil_imm(insn, ops, tg, false).map(|_| false),
        Vpermilpd => lift_vpermil_imm(insn, ops, tg, true).map(|_| false),
        Pshufb => {
            let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            vec_src_dispatch!(
                insn,
                ops,
                tg,
                reg_xmm,
                1,
                |idx| ops.push(IrOp::VPshufb { dst: d, a: d, idx }),
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
                |src| ops.push(IrOp::VAlignr {
                    dst: d,
                    a: d,
                    src,
                    imm
                }),
                |addr| ops.push(IrOp::VAlignrM { dst: d, addr, imm })
            );
            Ok(false)
        }
        // --- AES-NI (task-205). SSE 2-operand `op xmm1, xmm2/m128` (in-place, a=dst)
        // and VEX.128 3-operand `vop xmm1, xmm2, xmm3/m128` (a=op1, dst distinct,
        // 255:128 cleared). reg_xmm returns None for any ymm form → those stay deferred. ---
        Aesenc => lift_aes(insn, ops, tg, AesOp::Enc).map(|_| false),
        Aesdec => lift_aes(insn, ops, tg, AesOp::Dec).map(|_| false),
        Aesenclast => lift_aes(insn, ops, tg, AesOp::EncLast).map(|_| false),
        Aesdeclast => lift_aes(insn, ops, tg, AesOp::DecLast).map(|_| false),
        Vaesenc => lift_vaes(insn, ops, tg, AesOp::Enc).map(|_| false),
        Vaesdec => lift_vaes(insn, ops, tg, AesOp::Dec).map(|_| false),
        Vaesenclast => lift_vaes(insn, ops, tg, AesOp::EncLast).map(|_| false),
        Vaesdeclast => lift_vaes(insn, ops, tg, AesOp::DecLast).map(|_| false),
        Aesimc => lift_aes_imc(insn, ops, tg, false).map(|_| false),
        Vaesimc => lift_aes_imc(insn, ops, tg, true).map(|_| false),
        Aeskeygenassist => lift_aes_keygen(insn, ops, tg, false).map(|_| false),
        Vaeskeygenassist => lift_aes_keygen(insn, ops, tg, true).map(|_| false),
        // PCLMULQDQ (task-211). SSE in-place; VEX.128 3-operand clears bits 255:128.
        Pclmulqdq => lift_pclmul(insn, ops, tg).map(|_| false),
        Vpclmulqdq => lift_vpclmul(insn, ops, tg).map(|_| false),
        // MMX↔XMM bridge (SSE2, task-208). MMX aliases the low 64 bits of physical fpr[i].
        Movq2dq => {
            let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let src_mm = reg_mmx(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            ops.push(IrOp::Movq2dq { dst, src_mm });
            Ok(false)
        }
        Movdq2q => {
            let dst_mm = reg_mmx(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let src_xmm = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            ops.push(IrOp::Movdq2q { dst_mm, src_xmm });
            Ok(false)
        }
        // emms/femms mark the x87/MMX tag word empty; with no tag word modeled (and no
        // register-data change on hardware), this is a no-op.
        Emms | Femms => Ok(false),
        // --- SHA-NI (task-207). SSE 2-operand `sha... xmm1, xmm2/m128[, imm8]`
        // (in-place, a=dst); sha256rnds2 reads xmm0 implicitly at runtime. ---
        Sha256rnds2 => lift_sha(insn, ops, tg, ShaOp::Sha256Rnds2).map(|_| false),
        Sha256msg1 => lift_sha(insn, ops, tg, ShaOp::Sha256Msg1).map(|_| false),
        Sha256msg2 => lift_sha(insn, ops, tg, ShaOp::Sha256Msg2).map(|_| false),
        Sha1rnds4 => lift_sha(insn, ops, tg, ShaOp::Sha1Rnds4).map(|_| false),
        Sha1nexte => lift_sha(insn, ops, tg, ShaOp::Sha1NextE).map(|_| false),
        Sha1msg1 => lift_sha(insn, ops, tg, ShaOp::Sha1Msg1).map(|_| false),
        Sha1msg2 => lift_sha(insn, ops, tg, ShaOp::Sha1Msg2).map(|_| false),
        // --- GFNI (task-210). SSE 2-operand `op xmm1, xmm2/m128[, imm8]` (in-place)
        // and VEX.128 3-operand `vop xmm1, xmm2, xmm3/m128[, imm8]` (255:128 cleared). ---
        Gf2p8mulb => lift_gfni(insn, ops, tg, GfniOp::Mulb).map(|_| false),
        Gf2p8affineqb => lift_gfni(insn, ops, tg, GfniOp::AffineQb).map(|_| false),
        Gf2p8affineinvqb => lift_gfni(insn, ops, tg, GfniOp::AffineInvQb).map(|_| false),
        Vgf2p8mulb => lift_vgfni(insn, ops, tg, GfniOp::Mulb).map(|_| false),
        Vgf2p8affineqb => lift_vgfni(insn, ops, tg, GfniOp::AffineQb).map(|_| false),
        Vgf2p8affineinvqb => lift_vgfni(insn, ops, tg, GfniOp::AffineInvQb).map(|_| false),
        // --- SSSE3 psign (task-210). SSE 2-operand (in-place) + VEX.128 3-operand. ---
        Psignb => lift_psign(insn, ops, tg, 1).map(|_| false),
        Psignw => lift_psign(insn, ops, tg, 2).map(|_| false),
        Psignd => lift_psign(insn, ops, tg, 4).map(|_| false),
        Vpsignb => lift_vpsign(insn, ops, tg, 1).map(|_| false),
        Vpsignw => lift_vpsign(insn, ops, tg, 2).map(|_| false),
        Vpsignd => lift_vpsign(insn, ops, tg, 4).map(|_| false),
        Punpcklbw => lift_vunpack(insn, ops, tg, 1, false).map(|_| false),
        Punpcklwd => lift_vunpack(insn, ops, tg, 2, false).map(|_| false),
        Punpckldq => lift_vunpack(insn, ops, tg, 4, false).map(|_| false),
        Punpcklqdq => lift_vunpack(insn, ops, tg, 8, false).map(|_| false),
        Punpckhbw => lift_vunpack(insn, ops, tg, 1, true).map(|_| false),
        Punpckhwd => lift_vunpack(insn, ops, tg, 2, true).map(|_| false),
        Punpckhdq => lift_vunpack(insn, ops, tg, 4, true).map(|_| false),
        Punpckhqdq => lift_vunpack(insn, ops, tg, 8, true).map(|_| false),
        // SSE float unpacks (task-257): byte-identical to the integer interleave at the
        // matching lane width (4 = dword for *ps, 8 = qword for *pd), so they reuse the same
        // shared helper. dst == src1 (in-place), register or m128 src2.
        Unpcklps => lift_vunpack(insn, ops, tg, 4, false).map(|_| false),
        Unpckhps => lift_vunpack(insn, ops, tg, 4, true).map(|_| false),
        Unpcklpd => lift_vunpack(insn, ops, tg, 8, false).map(|_| false),
        Unpckhpd => lift_vunpack(insn, ops, tg, 8, true).map(|_| false),
        // VEX.128 interleave (task-195; mem src task-243): 3-operand `dst,a,b` (b reg or
        // 128-bit mem) + bits 255:128 cleared. reg_xmm returns None for the VEX.256/ymm
        // forms → those stay deferred.
        Vpunpcklbw => lift_vunpack_avx(insn, ops, tg, 1, false).map(|_| false),
        Vpunpcklwd => lift_vunpack_avx(insn, ops, tg, 2, false).map(|_| false),
        Vpunpckldq => lift_vunpack_avx(insn, ops, tg, 4, false).map(|_| false),
        Vpunpcklqdq => lift_vunpack_avx(insn, ops, tg, 8, false).map(|_| false),
        Vpunpckhbw => lift_vunpack_avx(insn, ops, tg, 1, true).map(|_| false),
        Vpunpckhwd => lift_vunpack_avx(insn, ops, tg, 2, true).map(|_| false),
        Vpunpckhdq => lift_vunpack_avx(insn, ops, tg, 4, true).map(|_| false),
        Vpunpckhqdq => lift_vunpack_avx(insn, ops, tg, 8, true).map(|_| false),
        // VEX float unpacks (task-257): the 3-operand form — byte-identical to the VEX integer
        // interleave at the matching lane width, so they reuse lift_vunpack_avx (which already
        // handles reg/m128 src2 and appends VZeroUpper for the 255:128 clear).
        Vunpcklps => lift_vunpack_avx(insn, ops, tg, 4, false).map(|_| false),
        Vunpckhps => lift_vunpack_avx(insn, ops, tg, 4, true).map(|_| false),
        Vunpcklpd => lift_vunpack_avx(insn, ops, tg, 8, false).map(|_| false),
        Vunpckhpd => lift_vunpack_avx(insn, ops, tg, 8, true).map(|_| false),
        Packuswb => lift_packuswb(insn, ops).map(|_| false),
        // Legacy SSE2 signed packs + pmaddwd (task-190; mem src task-243).
        Packsswb => lift_pack_signed(insn, ops, tg, 2).map(|_| false),
        Packssdw => lift_pack_signed(insn, ops, tg, 4).map(|_| false),
        Pmaddwd => lift_pmaddwd(insn, ops).map(|_| false),
        // VEX/EVEX saturating pack `vpack{ss,us}{wb,dw}` (task-195; 128-bit mem src task-243):
        // python3 hits vpackusdw. Register src (any width) or a 128-bit memory src2. VEX
        // upper-zeroing is implicit in the helper's set_vec (reg) or explicit (mem).
        Vpacksswb => lift_vpack(insn, ops, tg, 2, true).map(|_| false),
        Vpackuswb => lift_vpack(insn, ops, tg, 2, false).map(|_| false),
        Vpackssdw => lift_vpack(insn, ops, tg, 4, true).map(|_| false),
        Vpackusdw => lift_vpack(insn, ops, tg, 4, false).map(|_| false),
        Pinsrw => lift_pinsrw(insn, ops, tg).map(|_| false),
        Pextrw | Vpextrw => lift_pextrw(insn, ops, tg).map(|_| false),
        Pextrb | Vpextrb => lift_pextr(insn, ops, tg, 1).map(|_| false),
        // vextractps (VEX.128.66.0F3A.W0 17) extracts one 32-bit float lane
        // (imm8[1:0]) from the xmm source to a GPR32 (upper 32 bits zeroed) or a
        // dword in memory — semantically identical to vpextrd's 32-bit lane
        // extract, so it shares the same `VExtractLane { size: 4 }` path (task-168.6).
        Pextrd | Vpextrd | Vextractps => lift_pextr(insn, ops, tg, 4).map(|_| false),
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
        // movmskps/movmskpd (task-240): pack the packed-float sign bits into a GPR. The
        // 128-bit (SSE / VEX.128) form; a YMM source makes `reg_xmm` `None` → deferred.
        Movmskps | Vmovmskps | Movmskpd | Vmovmskpd => {
            let src = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let elem = if matches!(insn.mnemonic(), Movmskpd | Vmovmskpd) {
                8
            } else {
                4
            };
            let t = tg.fresh();
            ops.push(IrOp::VMoveMaskFp { dst: t, src, elem });
            let dst = lower_write_target(insn, 0, ops, tg)?;
            emit_write(ops, tg, dst, Val::Temp(t));
            Ok(false)
        }

        // --- AVX (VEX.128) — task-168.1/168.2. Reuse the u128 vector IR (already
        // 3-operand `dst,a,b`); a register destination also clears bits 255:128 of the
        // YMM via `VZeroUpper` (task-168.2). 256-bit/YMM forms fall through to
        // `unsupported` (`reg_xmm` rejects YMM) — deferred to AVX-256. ---
        // VEX forms (no EVEX mask) — `elem` is unused on the unmasked path, pass 4.
        // VEX non-temporal moves (task-246): a cache-bypass hint is a no-op in our coherent
        // model, so the stores (`vmovntdq`/`vmovntps`/`vmovntpd`, xmm -> [mem]) and the load
        // (`vmovntdqa`, [mem] -> xmm, with VEX.128 upper-zeroing) lower exactly like the
        // aligned `vmovdqa`/`vmovaps` path. libc's memmove emits `vmovntdq [rdi], xmm0`.
        Vmovdqa | Vmovdqu | Vmovaps | Vmovups | Vmovapd | Vmovupd | Vmovntdq | Vmovntps
        | Vmovntpd | Vmovntdqa => lift_vmov_avx(insn, ops, tg, 4).map(|_| false),
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
        // EVEX bitwise logic (task-168.5.2 unmasked, task-168.5.5 masked). The d/q suffix
        // sets the masking granularity (elem 4 vs 8).
        Vpxord => lift_evex_vlogic(insn, ops, tg, VLogicOp::Xor, 4).map(|_| false),
        Vpxorq => lift_evex_vlogic(insn, ops, tg, VLogicOp::Xor, 8).map(|_| false),
        Vpandd => lift_evex_vlogic(insn, ops, tg, VLogicOp::And, 4).map(|_| false),
        Vpandq => lift_evex_vlogic(insn, ops, tg, VLogicOp::And, 8).map(|_| false),
        Vpord => lift_evex_vlogic(insn, ops, tg, VLogicOp::Or, 4).map(|_| false),
        Vporq => lift_evex_vlogic(insn, ops, tg, VLogicOp::Or, 8).map(|_| false),
        Vpandnd => lift_evex_vlogic(insn, ops, tg, VLogicOp::Andn, 4).map(|_| false),
        Vpandnq => lift_evex_vlogic(insn, ops, tg, VLogicOp::Andn, 8).map(|_| false),
        Vpternlogd | Vpternlogq => lift_vpternlog(insn, ops, tg).map(|_| false),
        // AVX512-VPOPCNTDQ per-lane population count (task-195): register or memory src.
        Vpopcntd => lift_vpopcnt(insn, ops, tg, 4).map(|_| false),
        Vpopcntq => lift_vpopcnt(insn, ops, tg, 8).map(|_| false),
        // Opmask interleave `kunpck{bw,wd,dq}` (task-195): (a_low << half) | b_low.
        Kunpckbw => lift_kunpck(insn, ops, 8).map(|_| false),
        Kunpckwd => lift_kunpck(insn, ops, 16).map(|_| false),
        Kunpckdq => lift_kunpck(insn, ops, 32).map(|_| false),
        // Opmask bitwise logic `k{or,and,andn,xor,xnor}{b,w,d,q}` (task-195): glibc's
        // AVX-512 string routines combine per-chunk compare masks with these.
        Korb => lift_kbinop(insn, ops, VKLogicOp::Or, 8).map(|_| false),
        Korw => lift_kbinop(insn, ops, VKLogicOp::Or, 16).map(|_| false),
        Kord => lift_kbinop(insn, ops, VKLogicOp::Or, 32).map(|_| false),
        Korq => lift_kbinop(insn, ops, VKLogicOp::Or, 64).map(|_| false),
        Kandb => lift_kbinop(insn, ops, VKLogicOp::And, 8).map(|_| false),
        Kandw => lift_kbinop(insn, ops, VKLogicOp::And, 16).map(|_| false),
        Kandd => lift_kbinop(insn, ops, VKLogicOp::And, 32).map(|_| false),
        Kandq => lift_kbinop(insn, ops, VKLogicOp::And, 64).map(|_| false),
        Kandnb => lift_kbinop(insn, ops, VKLogicOp::Andn, 8).map(|_| false),
        Kandnw => lift_kbinop(insn, ops, VKLogicOp::Andn, 16).map(|_| false),
        Kandnd => lift_kbinop(insn, ops, VKLogicOp::Andn, 32).map(|_| false),
        Kandnq => lift_kbinop(insn, ops, VKLogicOp::Andn, 64).map(|_| false),
        Kxorb => lift_kbinop(insn, ops, VKLogicOp::Xor, 8).map(|_| false),
        Kxorw => lift_kbinop(insn, ops, VKLogicOp::Xor, 16).map(|_| false),
        Kxord => lift_kbinop(insn, ops, VKLogicOp::Xor, 32).map(|_| false),
        Kxorq => lift_kbinop(insn, ops, VKLogicOp::Xor, 64).map(|_| false),
        Kxnorb => lift_kbinop(insn, ops, VKLogicOp::Xnor, 8).map(|_| false),
        Kxnorw => lift_kbinop(insn, ops, VKLogicOp::Xnor, 16).map(|_| false),
        Kxnord => lift_kbinop(insn, ops, VKLogicOp::Xnor, 32).map(|_| false),
        Kxnorq => lift_kbinop(insn, ops, VKLogicOp::Xnor, 64).map(|_| false),
        Knotb => lift_knot(insn, ops, 8).map(|_| false),
        Knotw => lift_knot(insn, ops, 16).map(|_| false),
        Knotd => lift_knot(insn, ops, 32).map(|_| false),
        Knotq => lift_knot(insn, ops, 64).map(|_| false),
        // Opmask shift `kshift{l,r}{b,w,d,q}` (task-195): shift by imm8 within `width` bits.
        Kshiftlb => lift_kshift(insn, ops, 8, true).map(|_| false),
        Kshiftlw => lift_kshift(insn, ops, 16, true).map(|_| false),
        Kshiftld => lift_kshift(insn, ops, 32, true).map(|_| false),
        Kshiftlq => lift_kshift(insn, ops, 64, true).map(|_| false),
        Kshiftrb => lift_kshift(insn, ops, 8, false).map(|_| false),
        Kshiftrw => lift_kshift(insn, ops, 16, false).map(|_| false),
        Kshiftrd => lift_kshift(insn, ops, 32, false).map(|_| false),
        Kshiftrq => lift_kshift(insn, ops, 64, false).map(|_| false),
        // Two-table cross-lane permute `vpermt2{b,w,d,q}` (task-195): register src only.
        Vpermt2b => lift_vpermt2(insn, ops, tg, 1).map(|_| false),
        Vpermt2w => lift_vpermt2(insn, ops, tg, 2).map(|_| false),
        Vpermt2d => lift_vpermt2(insn, ops, tg, 4).map(|_| false),
        Vpermt2q => lift_vpermt2(insn, ops, tg, 8).map(|_| false),
        // Index-mode `vpermi2{b,w,d,q}` (task-195): old dst is the index, src1/src2 the
        // tables. python3 hits vpermi2w.
        Vpermi2b => lift_vpermi2(insn, ops, tg, 1).map(|_| false),
        Vpermi2w => lift_vpermi2(insn, ops, tg, 2).map(|_| false),
        Vpermi2d => lift_vpermi2(insn, ops, tg, 4).map(|_| false),
        Vpermi2q => lift_vpermi2(insn, ops, tg, 8).map(|_| false),
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
        // VEX-128 pmov{z,s}x (task-195): same extend + VEX upper-zeroing.
        Vpmovzxbw => lift_vpmovx(insn, ops, tg, 1, 2, false).map(|_| false),
        Vpmovzxbd => lift_vpmovx(insn, ops, tg, 1, 4, false).map(|_| false),
        Vpmovzxbq => lift_vpmovx(insn, ops, tg, 1, 8, false).map(|_| false),
        Vpmovzxwd => lift_vpmovx(insn, ops, tg, 2, 4, false).map(|_| false),
        Vpmovzxwq => lift_vpmovx(insn, ops, tg, 2, 8, false).map(|_| false),
        Vpmovzxdq => lift_vpmovx(insn, ops, tg, 4, 8, false).map(|_| false),
        Vpmovsxbw => lift_vpmovx(insn, ops, tg, 1, 2, true).map(|_| false),
        Vpmovsxbd => lift_vpmovx(insn, ops, tg, 1, 4, true).map(|_| false),
        Vpmovsxbq => lift_vpmovx(insn, ops, tg, 1, 8, true).map(|_| false),
        Vpmovsxwd => lift_vpmovx(insn, ops, tg, 2, 4, true).map(|_| false),
        Vpmovsxwq => lift_vpmovx(insn, ops, tg, 2, 8, true).map(|_| false),
        Vpmovsxdq => lift_vpmovx(insn, ops, tg, 4, 8, true).map(|_| false),
        // EVEX narrowing (truncating) move `vpmov{q,d,w}{d,w,b}` (task-195): pack each
        // src lane down to its low bytes; register dst only, masked/zeroing supported.
        Vpmovqd => lift_vpmov_narrow(insn, ops, tg, 8, 4).map(|_| false),
        Vpmovqw => lift_vpmov_narrow(insn, ops, tg, 8, 2).map(|_| false),
        Vpmovqb => lift_vpmov_narrow(insn, ops, tg, 8, 1).map(|_| false),
        Vpmovdw => lift_vpmov_narrow(insn, ops, tg, 4, 2).map(|_| false),
        Vpmovdb => lift_vpmov_narrow(insn, ops, tg, 4, 1).map(|_| false),
        Vpmovwb => lift_vpmov_narrow(insn, ops, tg, 2, 1).map(|_| false),
        // SSE2 pmullw / AVX vpmullw: per-lane low 16 bits of the 16×16 product (task-215).
        Pmullw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::MulLo16).map(|_| false),
        Vpmullw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::MulLo16).map(|_| false),
        // pmulhuw/pmulhw: per-lane high 16 bits of the unsigned/signed 16×16 product.
        Pmulhuw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::MulHiU16).map(|_| false),
        Vpmulhuw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::MulHiU16).map(|_| false),
        Pmulhw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::MulHiS16).map(|_| false),
        Vpmulhw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::MulHiS16).map(|_| false),
        // SSE4.1 pmulld / AVX vpmulld: per-lane low 32 bits of the 32×32 product.
        Pmulld => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MulLo32).map(|_| false),
        Vpmulld => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MulLo32).map(|_| false),
        // pmuludq/vpmuludq: unsigned low-dword × low-dword → full 64-bit lane (task-215).
        Pmuludq => lift_vpacked_bin(insn, ops, tg, 8, PackedBinOp::MulU32).map(|_| false),
        Vpmuludq => lift_vpacked_bin_avx(insn, ops, tg, 8, PackedBinOp::MulU32).map(|_| false),
        // pmuldq/vpmuldq: signed low-dword × low-dword → full 64-bit lane (task-215).
        Pmuldq => lift_vpacked_bin(insn, ops, tg, 8, PackedBinOp::MulS32).map(|_| false),
        Vpmuldq => lift_vpacked_bin_avx(insn, ops, tg, 8, PackedBinOp::MulS32).map(|_| false),
        // SSE4.1 dword min/max (the 16/8-bit forms are SSE2; these reuse the same ops).
        Pminsd => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MinS).map(|_| false),
        Pmaxsd => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MaxS).map(|_| false),
        Pminud => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MinU).map(|_| false),
        Pmaxud => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MaxU).map(|_| false),
        // SSE4.1 variable blend (mask = XMM0 lane MSBs) and imm8 rounding.
        Blendvps => lift_blendv(insn, ops, tg, 4).map(|_| false),
        Blendvpd => lift_blendv(insn, ops, tg, 8).map(|_| false),
        Pblendvb => lift_blendv(insn, ops, tg, 1).map(|_| false),
        // AVX VEX 4-operand variable blends (task-215, m128 src2 task-256): explicit mask
        // register; the m128 src2 form is the exact Celeste `vblendvps ...,[rip+disp32],...`.
        Vblendvps => lift_vblendv(insn, ops, tg, 4).map(|_| false),
        Vblendvpd => lift_vblendv(insn, ops, tg, 8).map(|_| false),
        Vpblendvb => lift_vblendv(insn, ops, tg, 1).map(|_| false),
        // AVX1 vector-mask conditional load/store (task-259): mask is a vector reg's
        // per-element sign bits; masked-off lanes never fault (Celeste libfmod blocker).
        Vmaskmovps => lift_vmaskmov(insn, ops, tg, 4).map(|_| false),
        Vmaskmovpd => lift_vmaskmov(insn, ops, tg, 8).map(|_| false),
        // SSE4.1 imm8 static blends `blendps`/`blendpd` (task-256): dst==src1; per lane,
        // imm8 bit i picks src2 lane i else keeps dst. Register or m128 src2.
        Blendps => lift_blendi(insn, ops, tg, 4).map(|_| false),
        Blendpd => lift_blendi(insn, ops, tg, 8).map(|_| false),
        // AVX `vblendps`/`vblendpd` (task-256): the VEX 3-operand imm8 static blend —
        // distinct merge base (vvvv) + VEX.128 upper-lane zeroing.
        Vblendps => lift_vblendi(insn, ops, tg, 4).map(|_| false),
        Vblendpd => lift_vblendi(insn, ops, tg, 8).map(|_| false),
        // VEX.128 `vpblendw` (task-195): per-word imm8 blend; python3 hits it. Register src.
        // SSE4.1 pblendw (task-215): imm8 word blend; dst is also src1, upper bits preserved.
        Pblendw => {
            let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let b = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?; // mem src2 deferred
            let imm = insn.immediate(2) as u8;
            ops.push(IrOp::VBlendW {
                dst,
                a: dst,
                b,
                imm,
            });
            Ok(false)
        }
        Vpblendw => lift_vpblendw(insn, ops).map(|_| false),
        Vpblendd => lift_vpblendd(insn, ops).map(|_| false),
        Roundps => lift_round(insn, ops, tg, FPrec::F32, false).map(|_| false),
        Roundpd => lift_round(insn, ops, tg, FPrec::F64, false).map(|_| false),
        Roundss => lift_round(insn, ops, tg, FPrec::F32, true).map(|_| false),
        Roundsd => lift_round(insn, ops, tg, FPrec::F64, true).map(|_| false),
        // VEX.128 `vround{ps,pd,ss,sd}` (task-242): the SSE4.1 round plus VEX upper-zeroing.
        // Packed forms round every lane (2-operand + imm8); scalar forms are 3-operand and
        // keep the upper bits of op1. Mono's Math.Round/Floor/Ceiling emit `vroundsd`.
        Vroundps => lift_vround(insn, ops, tg, FPrec::F32).map(|_| false),
        Vroundpd => lift_vround(insn, ops, tg, FPrec::F64).map(|_| false),
        Vroundss => lift_vround_scalar(insn, ops, tg, FPrec::F32).map(|_| false),
        Vroundsd => lift_vround_scalar(insn, ops, tg, FPrec::F64).map(|_| false),
        // EVEX scalar `vrndscale{ss,sd}` (task-195): for scale M=0 this is exactly a
        // 3-operand `round{ss,sd}` (same imm8 rounding-control bits). glibc's `floor`/
        // `ceil`/`rint` use it. Scaled (M≠0) and masked forms are deferred.
        Vrndscaless => lift_vrndscale(insn, ops, tg, FPrec::F32).map(|_| false),
        Vrndscalesd => lift_vrndscale(insn, ops, tg, FPrec::F64).map(|_| false),
        // SSE4.2 string-compare aggregation → ECX index + flags (task-168.5.4, mem-src2
        // task-195). SSE4.2 and its VEX-128 encoding are operand-identical (op0, op1, imm8)
        // and write only ECX + flags — no vector destination, so no upper-zeroing.
        Pcmpistri | Vpcmpistri => lift_pcmpstr_idx(insn, ops, tg, false).map(|_| false),
        Pcmpestri | Vpcmpestri => lift_pcmpstr_idx(insn, ops, tg, true).map(|_| false),
        // SSE4.2 string compare → mask in XMM0 (task-195). Same aggregation as the index
        // form; imm8[6] selects byte/word vs bit mask. VEX-128 is operand-identical.
        Pcmpistrm | Vpcmpistrm => lift_pcmpstr_mask(insn, ops, tg, false).map(|_| false),
        Pcmpestrm | Vpcmpestrm => lift_pcmpstr_mask(insn, ops, tg, true).map(|_| false),
        // SSE4.1 insertps (task-195): lane insert + zero mask; register or m32 source.
        Insertps => lift_insertps(insn, ops, tg).map(|_| false),
        // AVX vinsertps (task-255): the VEX 3-operand form — distinct merge base (vvvv) +
        // VEX.128 upper-lane zeroing; reuses the insert-and-zero semantics.
        Vinsertps => lift_vinsertps(insn, ops, tg).map(|_| false),
        // SSE4.1 dpps (task-195): single-precision dot product; register or m128 source.
        Dpps => lift_dpps(insn, ops, tg).map(|_| false),
        // SSE4.1 dppd (task-256): double-precision dot product; register or m128 source.
        Dppd => lift_dppd(insn, ops, tg).map(|_| false),
        // AVX `vdpps`/`vdppd` (task-256): the VEX 3-operand dot product — distinct merge
        // base (vvvv) + VEX.128 upper-lane zeroing; reuses the dpps()/dppd() helpers.
        Vdpps => lift_vdp(insn, ops, tg, FPrec::F32).map(|_| false),
        Vdppd => lift_vdp(insn, ops, tg, FPrec::F64).map(|_| false),
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
        Vpcmpeqq => lift_vpcmp_fixed_or_packed(insn, ops, tg, 8, PackedBinOp::CmpEq, 0, false)
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
        Vpcmpgtq => {
            lift_vpcmp_fixed_or_packed(insn, ops, tg, 8, PackedBinOp::CmpGt, 6, true).map(|_| false)
        }
        Vpminub => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::MinU).map(|_| false),
        Vpmaxub => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::MaxU).map(|_| false),
        // Dword packed min/max (SSE4.1 VEX + EVEX, task-195): perl/python3 hit vpminud.
        // Width-generic (128/256/512) + masked via lift_vpacked_bin_avx.
        Vpminud => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MinU).map(|_| false),
        Vpmaxud => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MaxU).map(|_| false),
        Vpminsd => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MinS).map(|_| false),
        Vpmaxsd => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MaxS).map(|_| false),
        // VEX packed-int sweep (task-260): saturating add/sub, rounding average, and the
        // byte/word min/max forms missing until now. All width-generic (xmm/ymm) + mem via
        // lift_vpacked_bin_avx, reusing the existing PackedBinOp primitives (jit == interp).
        Vpaddsb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::AddSatS).map(|_| false),
        Vpaddsw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::AddSatS).map(|_| false),
        Vpaddusb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::AddSatU).map(|_| false),
        Vpaddusw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::AddSatU).map(|_| false),
        Vpsubsb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::SubSatS).map(|_| false),
        Vpsubsw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::SubSatS).map(|_| false),
        Vpsubusb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::SubSatU).map(|_| false),
        Vpsubusw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::SubSatU).map(|_| false),
        Vpavgb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::AvgU).map(|_| false),
        Vpavgw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::AvgU).map(|_| false),
        Vpmaxsb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::MaxS).map(|_| false),
        Vpmaxsw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::MaxS).map(|_| false),
        Vpmaxuw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::MaxU).map(|_| false),
        Vpminsb => lift_vpacked_bin_avx(insn, ops, tg, 1, PackedBinOp::MinS).map(|_| false),
        Vpminsw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::MinS).map(|_| false),
        Vpminuw => lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::MinU).map(|_| false),
        // SSSE3 rounded-high multiply `pmulhrsw`/`vpmulhrsw` (task-260): per signed word,
        // bits [16:1] of the product, rounded. New PackedBinOp primitive; SSE + VEX.
        Pmulhrsw => lift_vpacked_bin(insn, ops, tg, 2, PackedBinOp::MulHiRoundedS16).map(|_| false),
        Vpmulhrsw => {
            lift_vpacked_bin_avx(insn, ops, tg, 2, PackedBinOp::MulHiRoundedS16).map(|_| false)
        }
        // VEX multiply-add `vpmaddwd`/`vpmaddubsw` (task-260): width-generic (xmm/ymm) +
        // reg/mem src2 via the shared exec_v_pmadd. `vpmaddwd` reuses the same core widened
        // from the legacy SSE2 form; `vpmaddubsw` is the SSSE3 unsigned×signed byte-pair
        // saturating multiply-add.
        Vpmaddwd => lift_vpmadd(insn, ops, tg, false).map(|_| false),
        Vpmaddubsw => lift_vpmadd(insn, ops, tg, true).map(|_| false),
        // SSSE3 legacy `pmaddubsw` (task-260): the SSE 2-operand form (dst == src1).
        Pmaddubsw => lift_pmaddubsw(insn, ops).map(|_| false),
        // Packed absolute value `vpabs{b,w,d,q}` (VEX/EVEX, task-195): any width, masked.
        Vpabsb => lift_vpabs(insn, ops, 1).map(|_| false),
        Vpabsw => lift_vpabs(insn, ops, 2).map(|_| false),
        Vpabsd => lift_vpabs(insn, ops, 4).map(|_| false),
        Vpabsq => lift_vpabs(insn, ops, 8).map(|_| false),
        // Masked EVEX unary lane ops (task-209): lzcnt / rotate-imm / conflict-detect.
        Vplzcntd => lift_vp_unary_lane(insn, ops, VpUnaryOp::Lzcnt, 4).map(|_| false),
        Vplzcntq => lift_vp_unary_lane(insn, ops, VpUnaryOp::Lzcnt, 8).map(|_| false),
        Vprold => lift_vp_unary_lane(insn, ops, VpUnaryOp::Rol, 4).map(|_| false),
        Vprolq => lift_vp_unary_lane(insn, ops, VpUnaryOp::Rol, 8).map(|_| false),
        Vpconflictd => lift_vp_unary_lane(insn, ops, VpUnaryOp::Conflict, 4).map(|_| false),
        Vpconflictq => lift_vp_unary_lane(insn, ops, VpUnaryOp::Conflict, 8).map(|_| false),
        // Masked EVEX blend `vpblendm{d,q}` (task-209): opmask is the blend control.
        Vpblendmd => lift_vp_blendm(insn, ops, tg, 4).map(|_| false),
        Vpblendmq => lift_vp_blendm(insn, ops, tg, 8).map(|_| false),
        // Masked EVEX 128-bit-lane shuffle (task-209): elem = masking granularity.
        Vshuff32x4 => lift_vshuf_lane(insn, ops, 4).map(|_| false),
        Vshuff64x2 => lift_vshuf_lane(insn, ops, 8).map(|_| false),
        // Masked EVEX per-qword unaligned byte gather `vpmultishiftqb` (VBMI, task-209).
        Vpmultishiftqb => lift_vp_multishift(insn, ops).map(|_| false),
        // EVEX-only 64-bit packed min/max (AVX-512, task-168.5 grind). 128-bit only.
        Vpmaxuq => lift_evex_packed_bin_128(insn, ops, tg, 8, PackedBinOp::MaxU).map(|_| false),
        Vpminuq => lift_evex_packed_bin_128(insn, ops, tg, 8, PackedBinOp::MinU).map(|_| false),
        Vpmaxsq => lift_evex_packed_bin_128(insn, ops, tg, 8, PackedBinOp::MaxS).map(|_| false),
        Vpminsq => lift_evex_packed_bin_128(insn, ops, tg, 8, PackedBinOp::MinS).map(|_| false),
        // AVX-512DQ 64-bit packed multiply-low `vpmullq` (task-195): width-generic
        // (128/256/512) register or memory src2, masked/zeroing. openssl/curl hit it.
        Vpmullq => lift_vpacked_bin_avx(insn, ops, tg, 8, PackedBinOp::MulLo64).map(|_| false),
        // EVEX vpcmp{,u}{b,w,d,q} → opmask (task-168.5 opmask subsystem).
        // EVEX vptestm/vptestnm → opmask (task-168.5.4, glibc AVX-512 strlen/memchr).
        Vptestmb => lift_vptest(insn, ops, tg, 1, false).map(|_| false),
        Vptestmw => lift_vptest(insn, ops, tg, 2, false).map(|_| false),
        Vptestmd => lift_vptest(insn, ops, tg, 4, false).map(|_| false),
        Vptestmq => lift_vptest(insn, ops, tg, 8, false).map(|_| false),
        Vptestnmb => lift_vptest(insn, ops, tg, 1, true).map(|_| false),
        Vptestnmw => lift_vptest(insn, ops, tg, 2, true).map(|_| false),
        Vptestnmd => lift_vptest(insn, ops, tg, 4, true).map(|_| false),
        Vptestnmq => lift_vptest(insn, ops, tg, 8, true).map(|_| false),
        Vpcmpb => lift_vpcmp(insn, ops, tg, 1, true).map(|_| false),
        Vpcmpw => lift_vpcmp(insn, ops, tg, 2, true).map(|_| false),
        Vpcmpd => lift_vpcmp(insn, ops, tg, 4, true).map(|_| false),
        Vpcmpq => lift_vpcmp(insn, ops, tg, 8, true).map(|_| false),
        Vpcmpub => lift_vpcmp(insn, ops, tg, 1, false).map(|_| false),
        Vpcmpuw => lift_vpcmp(insn, ops, tg, 2, false).map(|_| false),
        Vpcmpud => lift_vpcmp(insn, ops, tg, 4, false).map(|_| false),
        Vpcmpuq => lift_vpcmp(insn, ops, tg, 8, false).map(|_| false),
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
            // EVEX zmm (or any masked EVEX form) → the shared wide helper. Register idx
            // only (memory idx deferred for the 512-bit form). cal hits `vpshufb zmm`.
            if reg_zmm(insn, 0).is_some() || evex_is_masked(insn) {
                let (d, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
                let a = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
                let idx = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
                ops.push(IrOp::VPshufbWide {
                    dst: d,
                    a,
                    idx,
                    bytes,
                    writemask: evex_writemask(insn),
                    zeroing: insn.zeroing_masking(),
                });
                return Ok(false);
            }
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
            // VEX.128 3-operand `vpshufb dst, op1, op2`.
            let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            vec_src_dispatch!(
                insn,
                ops,
                tg,
                reg_xmm,
                2,
                // Register idx: shuffle op1's data directly; `VPshufb` reads `a` and
                // `idx` before writing `dst`, so an idx that aliases dst is safe
                // (task-203). No pre-copy of op1 into dst.
                |idx| ops.push(IrOp::VPshufb { dst: d, a, idx }),
                // Memory idx: `VPshufbM` shuffles `dst` in place, so op1 must be in
                // dst first. Memory can't alias a register, so this copy is safe.
                |addr| {
                    if d != a {
                        ops.push(IrOp::VMov { dst: d, src: a });
                    }
                    ops.push(IrOp::VPshufbM { dst: d, addr });
                }
            );
            ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
            Ok(false)
        }
        Vzeroupper | Vzeroall => {
            // vzeroall zeros the whole register (incl. low 128); vzeroupper preserves it.
            ops.push(IrOp::VZeroUpperAll {
                clear_low: insn.mnemonic() == Vzeroall,
            });
            Ok(false)
        }
        // AVX2 broadcast (task-168.3): replicate the low element across the dest.
        Vpbroadcastb => lift_broadcast(insn, ops, tg, 1).map(|_| false),
        Vpbroadcastw => lift_broadcast(insn, ops, tg, 2).map(|_| false),
        Vpbroadcastd => lift_broadcast(insn, ops, tg, 4).map(|_| false),
        Vpbroadcastq => lift_broadcast(insn, ops, tg, 8).map(|_| false),
        // Scalar float broadcast `vbroadcastss`/`vbroadcastsd` (AVX + EVEX, task-214):
        // replicate a 32/64-bit scalar across the dest — semantically vpbroadcastd/q.
        Vbroadcastss => lift_broadcast(insn, ops, tg, 4).map(|_| false),
        Vbroadcastsd => lift_broadcast(insn, ops, tg, 8).map(|_| false),
        // EVEX lane broadcast (task-214): replicate a 64/128/256-bit chunk across lanes.
        // openssl's v4 PRNG/crypto paths hit `vbroadcasti64x2` (previously trapped). The
        // `x` element size is the mask granularity (32→4, 64→8).
        Vbroadcastf32x2 | Vbroadcasti32x2 => {
            lift_broadcast_lane(insn, ops, tg, 8, 4).map(|_| false)
        }
        Vbroadcastf32x4 | Vbroadcasti32x4 => {
            lift_broadcast_lane(insn, ops, tg, 16, 4).map(|_| false)
        }
        Vbroadcastf64x2 | Vbroadcasti64x2 => {
            lift_broadcast_lane(insn, ops, tg, 16, 8).map(|_| false)
        }
        Vbroadcastf32x8 | Vbroadcasti32x8 => {
            lift_broadcast_lane(insn, ops, tg, 32, 4).map(|_| false)
        }
        Vbroadcastf64x4 | Vbroadcasti64x4 => {
            lift_broadcast_lane(insn, ops, tg, 32, 8).map(|_| false)
        }
        Vbroadcasti128 | Vbroadcastf128 => {
            lift_broadcast_lane(insn, ops, tg, 16, 16).map(|_| false)
        }
        // 128-bit lane insert / extract between XMM and YMM (task-168.3).
        Vinserti128 | Vinsertf128 => {
            let dst = reg_ymm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let src = reg_ymm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
            let hi = insn.immediate(3) & 1 != 0;
            if insn.op_kind(2) == OpKind::Memory {
                let addr = effective_address(insn, ops, tg)?;
                ops.push(IrOp::VInsert128M { dst, src, addr, hi });
                return Ok(false);
            }
            let ins = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
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
        // EVEX lane inserts (task-168.5.6): 128-bit (x4/x2) or 256-bit (x4) into ZMM/YMM.
        Vinserti32x4 | Vinsertf32x4 | Vinserti64x2 | Vinsertf64x2 => {
            lift_vinsert_wide(insn, ops, 1).map(|_| false)
        }
        Vinserti64x4 | Vinsertf64x4 | Vinserti32x8 | Vinsertf32x8 => {
            lift_vinsert_wide(insn, ops, 2).map(|_| false)
        }
        // EVEX lane extracts (task-195): 128-bit (x4/x2) or 256-bit (x8/x4) out of ZMM/YMM.
        Vextracti32x4 | Vextractf32x4 | Vextracti64x2 | Vextractf64x2 => {
            lift_vextract_wide(insn, ops, tg, 1).map(|_| false)
        }
        Vextracti64x4 | Vextractf64x4 | Vextracti32x8 | Vextractf32x8 => {
            lift_vextract_wide(insn, ops, tg, 2).map(|_| false)
        }
        // EVEX cross-lane align (task-168.5.6).
        Valignd => lift_valign(insn, ops, 4).map(|_| false),
        Valignq => lift_valign(insn, ops, 8).map(|_| false),
        // VEX packed shift-by-immediate (128 + 256), task-168.3.
        Vpsllw => lift_vpacked_shift_avx(insn, ops, tg, 2, false, false).map(|_| false),
        Vpslld => lift_vpacked_shift_avx(insn, ops, tg, 4, false, false).map(|_| false),
        Vpsllq => lift_vpacked_shift_avx(insn, ops, tg, 8, false, false).map(|_| false),
        Vpsrlw => lift_vpacked_shift_avx(insn, ops, tg, 2, true, false).map(|_| false),
        Vpsrld => lift_vpacked_shift_avx(insn, ops, tg, 4, true, false).map(|_| false),
        Vpsrlq => lift_vpacked_shift_avx(insn, ops, tg, 8, true, false).map(|_| false),
        Vpsraw => lift_vpacked_shift_avx(insn, ops, tg, 2, true, true).map(|_| false),
        Vpsrad => lift_vpacked_shift_avx(insn, ops, tg, 4, true, true).map(|_| false),
        // vpsraq: AVX-512 only (no VEX form) — arithmetic 64-bit right shift (task-215).
        Vpsraq => lift_vpacked_shift_avx(insn, ops, tg, 8, true, true).map(|_| false),
        // AVX2/AVX-512 per-element variable shifts `vp{sll,srl,sra}v{w,d,q}` (task-215).
        Vpsllvw => lift_vshift_var(insn, ops, 2, false, false).map(|_| false),
        Vpsllvd => lift_vshift_var(insn, ops, 4, false, false).map(|_| false),
        Vpsllvq => lift_vshift_var(insn, ops, 8, false, false).map(|_| false),
        Vpsrlvw => lift_vshift_var(insn, ops, 2, true, false).map(|_| false),
        Vpsrlvd => lift_vshift_var(insn, ops, 4, true, false).map(|_| false),
        Vpsrlvq => lift_vshift_var(insn, ops, 8, true, false).map(|_| false),
        Vpsravw => lift_vshift_var(insn, ops, 2, true, true).map(|_| false),
        Vpsravd => lift_vshift_var(insn, ops, 4, true, true).map(|_| false),
        Vpsravq => lift_vshift_var(insn, ops, 8, true, true).map(|_| false),

        // AVX2 cross-lane permutes (task-168.3). Register forms; memory sources
        // deferred (mirrors vinserti128).
        Vpermq | Vpermpd if insn.op_kind(2) == OpKind::Immediate8 => {
            // imm8 4-qword cross-lane permute (vpermq and vpermpd are identical on the
            // 4×64-bit lanes). Register OR memory source — the mem form loads 256 bits
            // into dst first (openssl rsaz signing emits `vpermq ymm,[mem],imm`).
            let dst = reg_ymm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
            let imm = insn.immediate8();
            let src = match reg_ymm(insn, 1) {
                Some(s) => s,
                None if insn.op_kind(1) == OpKind::Memory => {
                    let addr = effective_address(insn, ops, tg)?;
                    ops.push(IrOp::VLoadWide {
                        dst,
                        addr,
                        bytes: 32,
                    });
                    dst
                }
                None => return Err(unsupported_insn(insn)),
            };
            ops.push(IrOp::VPermq { dst, src, imm });
            Ok(false)
        }
        // Vector-index `vpermq` (VEX.256 / EVEX) — single-source cross-lane permute. The
        // imm8 form is matched above; python3 hits the EVEX-512 vector-index form.
        Vpermq => lift_vperm1(insn, ops, tg, 8).map(|_| false),
        Vpermd => {
            // EVEX-512 or masked → the shared single-source permute; VEX.256 → ymm fast path.
            if reg_zmm(insn, 0).is_some() || evex_is_masked(insn) {
                return lift_vperm1(insn, ops, tg, 4).map(|_| false);
            }
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
            // `VAlignr` reads `a` (high) and `src`=b (low) before writing dst, so a
            // register op2 aliasing dst is safe — no pre-copy of op1 into dst (task-203).
            ops.push(IrOp::VAlignr {
                dst: d,
                a,
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
        Vmovss => lift_vscalar_fmove(insn, ops, tg, FPrec::F32).map(|_| false),
        Vmovsd => lift_vscalar_fmove(insn, ops, tg, FPrec::F64).map(|_| false),
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
        // VEX forms are operand-identical and set the same flags (no vector dest → no
        // upper-zeroing), so they share the SSE lowering (task-195).
        Ucomiss | Comiss | Vucomiss | Vcomiss => {
            lift_float_cmp(insn, ops, tg, FPrec::F32).map(|_| false)
        }
        Ucomisd | Comisd | Vucomisd | Vcomisd => {
            lift_float_cmp(insn, ops, tg, FPrec::F64).map(|_| false)
        }
        Cmpss => lift_float_cmp_mask(insn, ops, tg, FPrec::F32, true).map(|_| false),
        Cmppd => lift_float_cmp_mask(insn, ops, tg, FPrec::F64, false).map(|_| false),
        Cmpps => lift_float_cmp_mask(insn, ops, tg, FPrec::F32, false).map(|_| false),
        // VEX 3-operand `vcmp{ss,sd,ps,pd}` (VEX.128 + VEX.256): op1 != dst, imm8
        // predicate is the last operand; VEX.128 zeroes 255:128, VEX.256 fills 255:128.
        Vcmpss => lift_vfloat_cmp_mask(insn, ops, tg, FPrec::F32, true).map(|_| false),
        Vcmpsd => lift_vfloat_cmp_mask(insn, ops, tg, FPrec::F64, true).map(|_| false),
        Vcmpps => lift_vfloat_cmp_mask(insn, ops, tg, FPrec::F32, false).map(|_| false),
        Vcmppd => lift_vfloat_cmp_mask(insn, ops, tg, FPrec::F64, false).map(|_| false),
        Cvtsi2ss => lift_cvt_from_int(insn, ops, tg, FPrec::F32).map(|_| false),
        Cvtsi2sd => lift_cvt_from_int(insn, ops, tg, FPrec::F64).map(|_| false),
        Vcvtsi2ss => lift_vcvt_from_int(insn, ops, tg, FPrec::F32, true).map(|_| false),
        Vcvtsi2sd => lift_vcvt_from_int(insn, ops, tg, FPrec::F64, true).map(|_| false),
        Vcvtusi2ss => lift_vcvt_from_int(insn, ops, tg, FPrec::F32, false).map(|_| false),
        Vcvtusi2sd => lift_vcvt_from_int(insn, ops, tg, FPrec::F64, false).map(|_| false),
        Cvttss2si | Vcvttss2si => lift_cvt_to_int(insn, ops, tg, FPrec::F32, true).map(|_| false),
        Cvtss2si | Vcvtss2si => lift_cvt_to_int(insn, ops, tg, FPrec::F32, false).map(|_| false),
        Cvttsd2si | Vcvttsd2si => lift_cvt_to_int(insn, ops, tg, FPrec::F64, true).map(|_| false),
        Cvtsd2si | Vcvtsd2si => lift_cvt_to_int(insn, ops, tg, FPrec::F64, false).map(|_| false),
        // AVX-512 unsigned conversions `cvt(t)s*2usi` (task-195).
        Vcvttss2usi => {
            lift_cvt_to_int_signed(insn, ops, tg, FPrec::F32, true, false).map(|_| false)
        }
        Vcvtss2usi => {
            lift_cvt_to_int_signed(insn, ops, tg, FPrec::F32, false, false).map(|_| false)
        }
        Vcvttsd2usi => {
            lift_cvt_to_int_signed(insn, ops, tg, FPrec::F64, true, false).map(|_| false)
        }
        Vcvtsd2usi => {
            lift_cvt_to_int_signed(insn, ops, tg, FPrec::F64, false, false).map(|_| false)
        }
        Cvtss2sd => lift_cvt_float(insn, ops, tg, FPrec::F32, FPrec::F64).map(|_| false),
        Cvtsd2ss => lift_cvt_float(insn, ops, tg, FPrec::F64, FPrec::F32).map(|_| false),
        // Packed float↔int converts `cvt*p*` (task-239). SSE + VEX.128; 256/512 deferred.
        Cvtdq2ps => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Dq2Ps, false).map(|_| false),
        Cvtps2dq => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Ps2Dq, false).map(|_| false),
        Cvttps2dq => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Tps2Dq, false).map(|_| false),
        Cvtdq2pd => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Dq2Pd, false).map(|_| false),
        Cvtps2pd => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Ps2Pd, false).map(|_| false),
        Cvtpd2ps => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Pd2Ps, false).map(|_| false),
        Cvtpd2dq => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Pd2Dq, false).map(|_| false),
        Cvttpd2dq => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Tpd2Dq, false).map(|_| false),
        Vcvtdq2ps => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Dq2Ps, true).map(|_| false),
        Vcvtps2dq => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Ps2Dq, true).map(|_| false),
        Vcvttps2dq => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Tps2Dq, true).map(|_| false),
        Vcvtdq2pd => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Dq2Pd, true).map(|_| false),
        Vcvtps2pd => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Ps2Pd, true).map(|_| false),
        Vcvtpd2ps => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Pd2Ps, true).map(|_| false),
        Vcvtpd2dq => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Pd2Dq, true).map(|_| false),
        Vcvttpd2dq => lift_packed_cvt(insn, ops, tg, PackedCvtKind::Tpd2Dq, true).map(|_| false),
        // VEX scalar float convert (task-195): 3-operand — bits 127:32/64 from op1,
        // converted low element from op2, and bits 255:128 zeroed.
        Vcvtss2sd => lift_vcvt_scalar(insn, ops, tg, FPrec::F32, FPrec::F64).map(|_| false),
        Vcvtsd2ss => lift_vcvt_scalar(insn, ops, tg, FPrec::F64, FPrec::F32).map(|_| false),
        Minss => lift_float_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F32, true).map(|_| false),
        Minsd => lift_float_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F64, true).map(|_| false),
        Minps => lift_float_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F32, false).map(|_| false),
        Minpd => lift_float_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F64, false).map(|_| false),
        Maxss => lift_float_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F32, true).map(|_| false),
        Maxsd => lift_float_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F64, true).map(|_| false),
        Maxps => lift_float_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F32, false).map(|_| false),
        Maxpd => lift_float_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F64, false).map(|_| false),
        // VEX-128 scalar/packed float arithmetic (task-195): 3-operand `dst = op(a, b)`.
        Vaddss => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Add, FPrec::F32, true).map(|_| false),
        Vaddsd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Add, FPrec::F64, true).map(|_| false),
        Vaddps => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Add, FPrec::F32, false).map(|_| false),
        Vaddpd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Add, FPrec::F64, false).map(|_| false),
        Vsubss => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Sub, FPrec::F32, true).map(|_| false),
        Vsubsd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Sub, FPrec::F64, true).map(|_| false),
        Vsubps => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Sub, FPrec::F32, false).map(|_| false),
        Vsubpd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Sub, FPrec::F64, false).map(|_| false),
        Vmulss => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Mul, FPrec::F32, true).map(|_| false),
        Vmulsd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Mul, FPrec::F64, true).map(|_| false),
        Vmulps => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Mul, FPrec::F32, false).map(|_| false),
        Vmulpd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Mul, FPrec::F64, false).map(|_| false),
        Vdivss => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Div, FPrec::F32, true).map(|_| false),
        Vdivsd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Div, FPrec::F64, true).map(|_| false),
        Vdivps => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Div, FPrec::F32, false).map(|_| false),
        Vdivpd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Div, FPrec::F64, false).map(|_| false),
        Vminss => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F32, true).map(|_| false),
        Vminsd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F64, true).map(|_| false),
        Vminps => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F32, false).map(|_| false),
        Vminpd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Min, FPrec::F64, false).map(|_| false),
        Vmaxss => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F32, true).map(|_| false),
        Vmaxsd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F64, true).map(|_| false),
        Vmaxps => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F32, false).map(|_| false),
        Vmaxpd => lift_vfloat_bin(insn, ops, tg, FloatBinOp::Max, FPrec::F64, false).map(|_| false),
        // SSE3 lane-combining packed float `h{add,sub}p` / `addsubp` (task-244): legacy
        // 2-operand + VEX.128 3-operand, register or 128-bit memory src. Mono/MonoGame
        // math emits `vhaddpd`. VEX forms clear bits 255:128.
        Haddps => lift_hfloat(insn, ops, tg, HFloatOp::HAdd, FPrec::F32).map(|_| false),
        Haddpd => lift_hfloat(insn, ops, tg, HFloatOp::HAdd, FPrec::F64).map(|_| false),
        Hsubps => lift_hfloat(insn, ops, tg, HFloatOp::HSub, FPrec::F32).map(|_| false),
        Hsubpd => lift_hfloat(insn, ops, tg, HFloatOp::HSub, FPrec::F64).map(|_| false),
        Addsubps => lift_hfloat(insn, ops, tg, HFloatOp::AddSub, FPrec::F32).map(|_| false),
        Addsubpd => lift_hfloat(insn, ops, tg, HFloatOp::AddSub, FPrec::F64).map(|_| false),
        Vhaddps => lift_vhfloat(insn, ops, tg, HFloatOp::HAdd, FPrec::F32).map(|_| false),
        Vhaddpd => lift_vhfloat(insn, ops, tg, HFloatOp::HAdd, FPrec::F64).map(|_| false),
        Vhsubps => lift_vhfloat(insn, ops, tg, HFloatOp::HSub, FPrec::F32).map(|_| false),
        Vhsubpd => lift_vhfloat(insn, ops, tg, HFloatOp::HSub, FPrec::F64).map(|_| false),
        Vaddsubps => lift_vhfloat(insn, ops, tg, HFloatOp::AddSub, FPrec::F32).map(|_| false),
        Vaddsubpd => lift_vhfloat(insn, ops, tg, HFloatOp::AddSub, FPrec::F64).map(|_| false),
        // SSSE3 packed-integer horizontal `ph{add,sub}{w,d,sw}` (task-247): legacy 2-operand
        // + VEX.128 3-operand, register or 128-bit memory src. Mono's managed/JIT'd code
        // emits `vphaddd`. The `sw` variants signed-saturate; VEX forms clear bits 255:128.
        Phaddw => lift_hint(insn, ops, tg, HIntOp::AddW).map(|_| false),
        Phaddd => lift_hint(insn, ops, tg, HIntOp::AddD).map(|_| false),
        Phaddsw => lift_hint(insn, ops, tg, HIntOp::AddSw).map(|_| false),
        Phsubw => lift_hint(insn, ops, tg, HIntOp::SubW).map(|_| false),
        Phsubd => lift_hint(insn, ops, tg, HIntOp::SubD).map(|_| false),
        Phsubsw => lift_hint(insn, ops, tg, HIntOp::SubSw).map(|_| false),
        Vphaddw => lift_vhint(insn, ops, tg, HIntOp::AddW).map(|_| false),
        Vphaddd => lift_vhint(insn, ops, tg, HIntOp::AddD).map(|_| false),
        Vphaddsw => lift_vhint(insn, ops, tg, HIntOp::AddSw).map(|_| false),
        Vphsubw => lift_vhint(insn, ops, tg, HIntOp::SubW).map(|_| false),
        Vphsubd => lift_vhint(insn, ops, tg, HIntOp::SubD).map(|_| false),
        Vphsubsw => lift_vhint(insn, ops, tg, HIntOp::SubSw).map(|_| false),
        // task-249: psadbw / vpsadbw — packed sum-of-absolute-differences of bytes
        // (66.0F.WIG F6). Not horizontal, but same operand shape as the ph* ops, so it
        // rides the `HIntOp::Sad` path through lift_hint/lift_vhint. VEX forms clear bits
        // 255:128.
        Psadbw => lift_hint(insn, ops, tg, HIntOp::Sad).map(|_| false),
        Vpsadbw => lift_vhint(insn, ops, tg, HIntOp::Sad).map(|_| false),
        Sqrtss => lift_float_unary(insn, ops, FloatUnOp::Sqrt, FPrec::F32, true).map(|_| false),
        Sqrtsd => lift_float_unary(insn, ops, FloatUnOp::Sqrt, FPrec::F64, true).map(|_| false),
        // VEX scalar sqrt (task-195): 3-operand — sqrt(op2 low), upper from op1, 255:128
        // cleared. python3 hits vsqrtsd.
        Vsqrtss => {
            lift_vfloat_unary_scalar(insn, ops, tg, FloatUnOp::Sqrt, FPrec::F32).map(|_| false)
        }
        Vsqrtsd => {
            lift_vfloat_unary_scalar(insn, ops, tg, FloatUnOp::Sqrt, FPrec::F64).map(|_| false)
        }
        // VEX packed sqrt (task-257): 2-operand — whole dst = sqrt(op1) lane-wise, 255:128
        // cleared. Register or m128 source.
        Vsqrtps => {
            lift_vfloat_unary_packed(insn, ops, tg, FloatUnOp::Sqrt, FPrec::F32).map(|_| false)
        }
        Vsqrtpd => {
            lift_vfloat_unary_packed(insn, ops, tg, FloatUnOp::Sqrt, FPrec::F64).map(|_| false)
        }
        // Reciprocal-sqrt / reciprocal (task-257) — single-precision only, exact IEEE
        // 1.0/sqrt(x) / 1.0/x (see FloatUnOp docs for the approximation choice). Celeste's
        // c5 fa 52 d0 = `vrsqrtss xmm2, xmm0, xmm0` was the concrete blocker.
        Vrsqrtss => {
            lift_vfloat_unary_scalar(insn, ops, tg, FloatUnOp::Rsqrt, FPrec::F32).map(|_| false)
        }
        Vrcpss => {
            lift_vfloat_unary_scalar(insn, ops, tg, FloatUnOp::Rcp, FPrec::F32).map(|_| false)
        }
        Vrsqrtps => {
            lift_vfloat_unary_packed(insn, ops, tg, FloatUnOp::Rsqrt, FPrec::F32).map(|_| false)
        }
        Vrcpps => {
            lift_vfloat_unary_packed(insn, ops, tg, FloatUnOp::Rcp, FPrec::F32).map(|_| false)
        }
        // FMA3 `vf[n]m{add,sub}{132,213,231}{ss,sd,ps,pd}` (task-201). python3 numerics.
        Vfmadd132ss => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, true, false, false, 0).map(|_| false)
        }
        Vfmadd132sd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, true, false, false, 0).map(|_| false)
        }
        Vfmadd132ps => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, false, false, false, 0).map(|_| false)
        }
        Vfmadd132pd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, false, false, false, 0).map(|_| false)
        }
        Vfmadd213ss => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, true, false, false, 0).map(|_| false)
        }
        Vfmadd213sd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, true, false, false, 0).map(|_| false)
        }
        Vfmadd213ps => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, false, false, false, 0).map(|_| false)
        }
        Vfmadd213pd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, false, false, false, 0).map(|_| false)
        }
        Vfmadd231ss => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, true, false, false, 0).map(|_| false)
        }
        Vfmadd231sd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, true, false, false, 0).map(|_| false)
        }
        Vfmadd231ps => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, false, false, false, 0).map(|_| false)
        }
        Vfmadd231pd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, false, false, false, 0).map(|_| false)
        }
        Vfmsub132ss => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, true, false, true, 0).map(|_| false)
        }
        Vfmsub132sd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, true, false, true, 0).map(|_| false)
        }
        Vfmsub132ps => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, false, false, true, 0).map(|_| false)
        }
        Vfmsub132pd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, false, false, true, 0).map(|_| false)
        }
        Vfmsub213ss => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, true, false, true, 0).map(|_| false)
        }
        Vfmsub213sd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, true, false, true, 0).map(|_| false)
        }
        Vfmsub213ps => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, false, false, true, 0).map(|_| false)
        }
        Vfmsub213pd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, false, false, true, 0).map(|_| false)
        }
        Vfmsub231ss => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, true, false, true, 0).map(|_| false)
        }
        Vfmsub231sd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, true, false, true, 0).map(|_| false)
        }
        Vfmsub231ps => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, false, false, true, 0).map(|_| false)
        }
        Vfmsub231pd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, false, false, true, 0).map(|_| false)
        }
        Vfnmadd132ss => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, true, true, false, 0).map(|_| false)
        }
        Vfnmadd132sd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, true, true, false, 0).map(|_| false)
        }
        Vfnmadd132ps => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, false, true, false, 0).map(|_| false)
        }
        Vfnmadd132pd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, false, true, false, 0).map(|_| false)
        }
        Vfnmadd213ss => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, true, true, false, 0).map(|_| false)
        }
        Vfnmadd213sd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, true, true, false, 0).map(|_| false)
        }
        Vfnmadd213ps => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, false, true, false, 0).map(|_| false)
        }
        Vfnmadd213pd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, false, true, false, 0).map(|_| false)
        }
        Vfnmadd231ss => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, true, true, false, 0).map(|_| false)
        }
        Vfnmadd231sd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, true, true, false, 0).map(|_| false)
        }
        Vfnmadd231ps => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, false, true, false, 0).map(|_| false)
        }
        Vfnmadd231pd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, false, true, false, 0).map(|_| false)
        }
        Vfnmsub132ss => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, true, true, true, 0).map(|_| false)
        }
        Vfnmsub132sd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, true, true, true, 0).map(|_| false)
        }
        Vfnmsub132ps => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, false, true, true, 0).map(|_| false)
        }
        Vfnmsub132pd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, false, true, true, 0).map(|_| false)
        }
        Vfnmsub213ss => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, true, true, true, 0).map(|_| false)
        }
        Vfnmsub213sd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, true, true, true, 0).map(|_| false)
        }
        Vfnmsub213ps => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, false, true, true, 0).map(|_| false)
        }
        Vfnmsub213pd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, false, true, true, 0).map(|_| false)
        }
        Vfnmsub231ss => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, true, true, true, 0).map(|_| false)
        }
        Vfnmsub231sd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, true, true, true, 0).map(|_| false)
        }
        Vfnmsub231ps => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, false, true, true, 0).map(|_| false)
        }
        Vfnmsub231pd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, false, true, true, 0).map(|_| false)
        }
        // FMA alternating-sign family (task-261): fmaddsub = even lanes SUBTRACT z / odd
        // ADD z; fmsubadd = even ADD / odd SUBTRACT. Packed only (xmm+ymm); alt_sign 1/2
        // overrides the per-lane add sign. neg_prod/neg_add stay false (base FMA).
        Vfmaddsub132ps => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, false, false, false, 1).map(|_| false)
        }
        Vfmaddsub132pd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, false, false, false, 1).map(|_| false)
        }
        Vfmaddsub213ps => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, false, false, false, 1).map(|_| false)
        }
        Vfmaddsub213pd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, false, false, false, 1).map(|_| false)
        }
        Vfmaddsub231ps => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, false, false, false, 1).map(|_| false)
        }
        Vfmaddsub231pd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, false, false, false, 1).map(|_| false)
        }
        Vfmsubadd132ps => {
            lift_fma(insn, ops, tg, 132, FPrec::F32, false, false, false, 2).map(|_| false)
        }
        Vfmsubadd132pd => {
            lift_fma(insn, ops, tg, 132, FPrec::F64, false, false, false, 2).map(|_| false)
        }
        Vfmsubadd213ps => {
            lift_fma(insn, ops, tg, 213, FPrec::F32, false, false, false, 2).map(|_| false)
        }
        Vfmsubadd213pd => {
            lift_fma(insn, ops, tg, 213, FPrec::F64, false, false, false, 2).map(|_| false)
        }
        Vfmsubadd231ps => {
            lift_fma(insn, ops, tg, 231, FPrec::F32, false, false, false, 2).map(|_| false)
        }
        Vfmsubadd231pd => {
            lift_fma(insn, ops, tg, 231, FPrec::F64, false, false, false, 2).map(|_| false)
        }
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

        Push => lift_push(insn, ops, tg, mode).map(|_| false),
        Pop => lift_pop(insn, ops, tg, mode).map(|_| false),

        // --- control flow: ends the block ---
        Jmp => {
            let target = branch_target(insn, ops, tg, mode)?;
            ops.push(IrOp::Jump { target });
            Ok(true)
        }
        Call => {
            let slot = call_ret_slot(insn, mode)?;
            let target = branch_target(insn, ops, tg, mode)?;
            ops.push(IrOp::Call {
                target,
                return_addr: mask_pc(insn.next_ip(), mode),
                slot,
                wrap_sp: mode.wraps_32(),
            });
            Ok(true)
        }
        Ret => {
            let slot = call_ret_slot(insn, mode)?;
            // `ret imm16` adds a caller-cleanup immediate to the stack pointer after
            // popping EIP; plain `ret` has no immediate (pop_extra = 0).
            let pop_extra = if insn.op_count() > 0 {
                insn.immediate16()
            } else {
                0
            };
            ops.push(IrOp::Ret {
                slot,
                pop_extra,
                wrap_sp: mode.wraps_32(),
            });
            Ok(true)
        }
        // leave = mov rsp, rbp; pop rbp. A 66h override (`Leavew`) makes the pop
        // 16-bit: BP is written 16-bit (upper bits preserved) and SP advances by 2,
        // while RSP itself is still a full-width stack-pointer write (§16).
        Leave => {
            let stk = if insn.code() == Code::Leavew {
                2
            } else {
                stack_slot(mode)
            };
            let rbp = read_reg(Reg::Rbp, ops, tg);
            let val = tg.fresh();
            ops.push(IrOp::Load {
                dst: val,
                addr: rbp,
                size: stk,
            });
            let new_rsp = tg.fresh();
            ops.push(IrOp::Add {
                dst: new_rsp,
                a: rbp,
                b: Val::Imm(stk as u64),
                size: 8,
                set_flags: FlagMask::NONE,
            });
            ops.push(IrOp::WriteReg {
                reg: Reg::Rbp,
                src: Val::Temp(val),
                size: stk,
            });
            // A 4-byte RSP write in Compat32 zero-extends → ESP wraps mod 2^32.
            ops.push(IrOp::WriteReg {
                reg: Reg::Rsp,
                src: Val::Temp(new_rsp),
                size: sp_write_size(mode),
            });
            Ok(false)
        }
        Syscall => {
            // The real AMD64 `syscall` instruction latches RCX/R11.
            ops.push(IrOp::Syscall { is_amd64: true });
            Ok(true)
        }
        Hlt => {
            ops.push(IrOp::Hlt);
            Ok(true)
        }

        // Port I/O — trap out to the embedder (§5.2), the machine counterpart of
        // MMIO. `in`/`out` in both the imm8 and `dx` forms and all three access
        // widths (1/2/4). `ins`/`outs` (string port I/O, incl. `rep` forms) are
        // deliberately NOT lifted: no consumer exists (they matter only to BIOS-era
        // block-device drivers), and a correct per-element trap-out would need its
        // own restartable-loop machinery. They fall through to `UnknownInstruction`,
        // which names the exact opcode to add if one ever surfaces.
        In => lift_port_io(insn, ops, tg, false).map(|_| true),
        Out => lift_port_io(insn, ops, tg, true).map(|_| true),

        // Instructions that architecturally *raise an exception* rather than
        // executing: they are not lift gaps (so must NOT become
        // `UnknownInstruction`) — the guest deliberately faults here. Each ends the
        // block with RIP on the instruction so the dispatcher reports the vector.
        //   ud2  (0f 0b)  -> #UD (invalid opcode, vector 6)
        //   int3 (cc)     -> #BP (breakpoint,     vector 3)
        //   int1 (f1)     -> #DB (debug,          vector 1)
        // #UD is a fault (RIP stays on ud2); #BP/#DB are traps (RIP resumes past the
        // instruction) — `advance` carries that, HW-accurately and host-independently.
        Ud2 => {
            ops.push(IrOp::Trap {
                vector: 6,
                advance: 0,
            });
            Ok(true)
        }
        Int3 => {
            ops.push(IrOp::Trap {
                vector: 3,
                advance: insn.len() as u8,
            });
            Ok(true)
        }
        Int1 => {
            ops.push(IrOp::Trap {
                vector: 1,
                advance: insn.len() as u8,
            });
            Ok(true)
        }
        // `int imm8` (`CD ib`). Vector `0x80` is the Linux i386 syscall gate: surface
        // it as `Exit::Syscall` exactly like `syscall`/`sysenter` in long mode — the
        // embedder inspects `cpu_mode()` to pick the i386 vs x86-64 ABI (exit.rs). RIP
        // already advances past the 2-byte instruction (`block_end`/`guest_end`), the
        // same convention `syscall` uses. Any other `int n` is a guest-raised software
        // interrupt: model it as a trap to that vector (like `int3`/`int1`), a #GP/IVT
        // delivery is out of scope (deferred to TASK-199).
        Int => {
            if insn.immediate8() == 0x80 {
                // i386 `int 0x80` gate: same `Exit::Syscall`, but the i386 ABI
                // passes args in ECX/… so it must NOT clobber RCX/R11.
                ops.push(IrOp::Syscall { is_amd64: false });
            } else {
                ops.push(IrOp::Trap {
                    vector: insn.immediate8(),
                    advance: insn.len() as u8,
                });
            }
            Ok(true)
        }

        _ => {
            if let Some(cond) = jcc_cond(insn.mnemonic()) {
                ops.push(IrOp::Branch {
                    cond,
                    taken: mask_pc(insn.near_branch_target(), mode),
                    fallthrough: mask_pc(insn.next_ip(), mode),
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
pub(crate) enum BinOp {
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
pub(crate) fn rmw_of_binop(op: BinOp) -> Option<RmwOp> {
    match op {
        BinOp::Add => Some(RmwOp::Add),
        BinOp::Sub => Some(RmwOp::Sub),
        BinOp::And => Some(RmwOp::And),
        BinOp::Or => Some(RmwOp::Or),
        BinOp::Xor => Some(RmwOp::Xor),
        _ => None,
    }
}

pub(crate) fn mk_binop(op: BinOp, dst: u32, a: Val, b: Val, size: u8, set_flags: FlagMask) -> IrOp {
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

/// Define a vector/opmask register-index extractor: `Some(index)` when operand
/// `op_idx` is a register of the given class, else `None`. Indices are relative to
/// the class base (XMM0/YMM0/ZMM0/K0), so EVEX high regs (16–31) come through
/// (task-170.3 consolidation of four near-identical extractors).
macro_rules! reg_extractor {
    ($(#[$m:meta])* $name:ident, $pred:ident, $base:ident) => {
        $(#[$m])*
        pub(crate) fn $name(insn: &Instruction, op_idx: u32) -> Option<u8> {
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
reg_extractor!(
    /// MMX register index (mm0–mm7) for an operand (task-208). MMX aliases the low 64
    /// bits of the *physical* x87 register `fpr[i]`.
    reg_mmx, is_mm, MM0
);

/// A vector operand's `(register index, byte width)` — XMM=16, YMM=32, ZMM=64.
pub(crate) fn vec_operand(insn: &Instruction, op_idx: u32) -> Option<(u8, u16)> {
    if let Some(z) = reg_zmm(insn, op_idx) {
        Some((z, 64))
    } else if let Some(y) = reg_ymm(insn, op_idx) {
        Some((y, 32))
    } else {
        reg_xmm(insn, op_idx).map(|x| (x, 16))
    }
}

/// Vector register index for an XMM *or* YMM operand (they share the 0–15 file).
pub(crate) fn reg_vec(insn: &Instruction, op_idx: u32) -> Option<u8> {
    reg_xmm(insn, op_idx).or_else(|| reg_ymm(insn, op_idx))
}

/// Register index of a vector operand (XMM/YMM/ZMM, 0–31), dropping the width — the
/// register-vs-memory `$ext` for [`vec_src_dispatch!`] on EVEX ops (task-195).
pub(crate) fn vec_operand_reg(insn: &Instruction, op_idx: u32) -> Option<u8> {
    vec_operand(insn, op_idx).map(|(r, _)| r)
}

/// True if an EVEX instruction carries a write-mask (k1–k7) or zeroing. Such forms
/// need per-element predication we don't yet lift — callers reject them for now
/// (task-168.5, unmasked-first).
pub(crate) fn evex_is_masked(insn: &Instruction) -> bool {
    insn.op_mask() != Register::None || insn.zeroing_masking()
}

/// The EVEX write-mask register index (k1–k7), or `None` for unmasked (k0/none).
pub(crate) fn evex_writemask(insn: &Instruction) -> Option<u8> {
    let r = insn.op_mask();
    if r == Register::None || r == Register::K0 {
        None
    } else {
        Some((r as u32 - Register::K0 as u32) as u8)
    }
}

/// Mask covering the low `size` bytes.
pub(crate) fn size_mask(size: u8) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

// --- operand lowering (§7.1) ---

/// Reduce a SOURCE operand to a `Val` (reads reg / immediate / loads memory).
pub(crate) fn lower_read(
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
pub(crate) fn scalar_mem_size(insn: &Instruction, op_idx: u32) -> Result<u8, LiftError> {
    let size = operand_size(insn, op_idx);
    if matches!(size, 1 | 2 | 4 | 8) {
        Ok(size)
    } else {
        Err(unsupported_insn(insn))
    }
}

/// Reduce a DESTINATION operand to a write handle (reg or memory address).
pub(crate) fn lower_write_target(
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
pub(crate) fn high_byte_parent(reg: Register) -> Option<Reg> {
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
pub(crate) fn effective_address(
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
pub(crate) fn effective_address_no_segment(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<Val, LiftError> {
    let base = insn.memory_base();
    let index = insn.memory_index();
    let scale = insn.memory_index_scale();
    let disp = insn.memory_displacement64();

    // RIP-relative addressing exists only in 64-bit mode (§17.5). iced already folded
    // RIP+disp into an absolute address. Under a 32-bit address-size override (67h)
    // *in long mode* iced reports `EIP` (not `RIP`) and folds the same way; truncate
    // the folded value to 32 bits, matching the register-form wrap below. A 32-bit
    // (`Compat32`) decode NEVER yields an IP-relative operand — its ModRM disp32 form
    // is absolute (`base == None`) — so an EIP/RIP base under a 32-bit decode is a
    // decoder invariant break, not a real address: fail loudly rather than compute
    // garbage.
    if base == Register::RIP {
        debug_assert_ne!(
            insn.code_size(),
            CodeSize::Code32,
            "RIP-relative operand under a 32-bit (Compat32) decode",
        );
        return Ok(Val::Imm(disp));
    }
    if base == Register::EIP {
        debug_assert_ne!(
            insn.code_size(),
            CodeSize::Code32,
            "EIP-relative operand under a 32-bit (Compat32) decode",
        );
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

    // Address-size truncation (§17.5). The effective address wraps modulo the
    // addressing width, which iced encodes in the base/index register *size*:
    //   - 4-byte regs (EBX, not RBX) → 32-bit addressing, wrap mod 2^32. This is both
    //     `Compat32`'s default and long mode's 0x67 override.
    //   - 2-byte regs (BX/BP/SI/DI) → 16-bit addressing (0x67 in `Compat32`; classic
    //     ModRM forms with no SIB), wrap mod 2^16.
    //   - 8-byte regs (or a pure absolute disp) → 64-bit, no wrap.
    // A pure `[disp]` absolute (base == index == None) needs no mask: iced hands a
    // displacement already sized to the decode width (≤32 bits for disp32, ≤16 for the
    // 67h disp16 form).
    let addr_mask = if base.size() == 2 || index.size() == 2 {
        Some(0xFFFFu64)
    } else if base.size() == 4 || index.size() == 4 {
        Some(0xFFFF_FFFFu64)
    } else {
        None
    };
    if let Some(mask) = addr_mask {
        return Ok(match addr {
            Val::Imm(v) => Val::Imm(v & mask),
            a => {
                let t = tg.fresh();
                ops.push(IrOp::And {
                    dst: t,
                    a,
                    b: Val::Imm(mask),
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
pub(crate) fn with_segment(
    insn: &Instruction,
    addr: Val,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Val {
    let seg = match insn.segment_prefix() {
        Register::FS => Reg::FsBase,
        Register::GS => Reg::GsBase,
        _ => return addr,
    };
    let base = read_reg(seg, ops, tg);
    add_addr(addr, base, ops, tg)
}

/// Emit a non-flag-setting 64-bit address addition.
pub(crate) fn add_addr(a: Val, b: Val, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val {
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
pub(crate) fn read_reg(reg: Reg, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val {
    let t = tg.fresh();
    ops.push(IrOp::ReadReg { dst: t, reg });
    Val::Temp(t)
}

pub(crate) fn emit_write(ops: &mut Vec<IrOp>, tg: &mut TempGen, target: WriteTarget, value: Val) {
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
pub(crate) fn alu_none(ops: &mut Vec<IrOp>, tg: &mut TempGen, mk: impl FnOnce(u32) -> IrOp) -> Val {
    let t = tg.fresh();
    ops.push(mk(t));
    Val::Temp(t)
}

/// Target `Val` for a jmp/call: an immediate for a near (rel) branch, otherwise the
/// value of the indirect register/memory operand.
pub(crate) fn branch_target(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mode: CpuMode,
) -> Result<Val, LiftError> {
    match insn.op_kind(0) {
        // Direct near branch: iced resolves the target already truncated to the
        // decode bitness, so a 32-bit decode yields a target < 2^32 (mask is a no-op
        // but pins the invariant); no runtime op needed.
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
            Ok(Val::Imm(mask_pc(insn.near_branch_target(), mode)))
        }
        // Indirect near branch: the loaded target must be truncated to the effective
        // operand width. A 66h `jmp r/m16` (`Jmp_rm16`, Compat32/16-bit only — long
        // mode forces 64-bit and ignores 66h) takes only the low 16 bits of the
        // operand as the new (E)IP; the wider forms wrap mod 2^32 in Compat32.
        _ => {
            let target = lower_read(insn, 0, ops, tg)?;
            let width_mask = match operand_size(insn, 0) {
                2 => Some(0xFFFFu64),
                _ if mode.wraps_32() => Some(0xFFFF_FFFF),
                _ => None,
            };
            if let Some(m) = width_mask {
                let masked = tg.fresh();
                ops.push(IrOp::And {
                    dst: masked,
                    a: target,
                    b: Val::Imm(m),
                    size: 8,
                    set_flags: FlagMask::NONE,
                });
                Ok(Val::Temp(masked))
            } else {
                Ok(target)
            }
        }
    }
}

// --- small helpers ---

/// Map an iced register (any width GPR) to our `Reg`. `None` for high-byte
/// registers (AH/BH/CH/DH, which write bits 8–15 and can't be expressed by the
/// 64-bit `Reg` enum) and non-GPRs — the caller turns `None` into `Unsupported`
/// rather than mis-lowering to the low byte.
pub(crate) fn iced_to_reg(reg: Register) -> Option<Reg> {
    if matches!(
        reg,
        Register::AH | Register::BH | Register::CH | Register::DH
    ) {
        return None;
    }
    iced_gpr_index(reg).map(Reg::from_gpr_index)
}

pub(crate) fn is_immediate(kind: OpKind) -> bool {
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
pub(crate) fn operand_size(insn: &Instruction, op_idx: u32) -> u8 {
    match insn.op_kind(op_idx) {
        OpKind::Register => insn.op_register(op_idx).size() as u8,
        OpKind::Memory => insn.memory_size().size() as u8,
        _ => 0,
    }
}

/// Width of a binary operation = size of operand 0 (the destination), falling back
/// to operand 1 for the rare all-immediate/implicit form.
pub(crate) fn operation_size(insn: &Instruction) -> u8 {
    let s = operand_size(insn, 0);
    if s != 0 {
        s
    } else {
        operand_size(insn, 1)
    }
}

/// push/pop transfer size. iced already reflects the effective operand size in the
/// operand width: long-mode default 8 (66h → 2), Compat32 default 4 (66h → 2). The
/// zero fallback covers immediate/implicit forms with no operand width: a 66h
/// `push imm` (`Push_imm16`/`Pushw_imm8`) transfers 2 bytes; everything else uses the
/// mode default.
pub(crate) fn push_pop_size(insn: &Instruction, mode: CpuMode) -> u8 {
    let s = operand_size(insn, 0);
    if s != 0 {
        return s;
    }
    // `push imm` carries no operand width; the 16-bit (66h) forms push 2 bytes.
    if matches!(insn.code(), Code::Push_imm16 | Code::Pushw_imm8) {
        return 2;
    }
    stack_slot(mode)
}

/// Default stack-frame width for the mode: 8 in long mode, 4 in Compat32.
pub(crate) fn stack_slot(mode: CpuMode) -> u8 {
    if mode.wraps_32() {
        4
    } else {
        8
    }
}

/// Width of a full-width RSP/ESP write: 8 in long mode (leaves RSP intact), 4 in
/// Compat32 (zero-extends → ESP wraps mod 2^32 via the central GPR write path).
pub(crate) fn sp_write_size(mode: CpuMode) -> u8 {
    stack_slot(mode)
}

/// Truncate a computed PC/return address to the mode's pointer width (Compat32: mod
/// 2^32). Long mode passes through. Used for direct-branch targets and return
/// addresses, which are `Val::Imm` literals resolved at lift time.
pub(crate) fn mask_pc(addr: u64, mode: CpuMode) -> u64 {
    if mode.wraps_32() {
        addr & 0xFFFF_FFFF
    } else {
        addr
    }
}

/// Emit a mod-2^32 mask on a freshly-computed stack pointer temp (Compat32 only), so
/// it is a valid 32-bit address before it is used as a store address (§16).
pub(crate) fn emit_sp_wrap(ops: &mut Vec<IrOp>, sp: Temp, mode: CpuMode) {
    if mode.wraps_32() {
        ops.push(IrOp::And {
            dst: sp,
            a: Val::Temp(sp),
            b: Val::Imm(0xFFFF_FFFF),
            size: 8,
            set_flags: FlagMask::NONE,
        });
    }
}

/// Stack-frame width pushed/popped by `call`/`ret`: the effective operand size (8 in
/// long mode, 4 in Compat32; a 66h override makes it 2). The rare 66h operand-size-16
/// near call/ret would truncate EIP mod 2^16 — §17.7: reject it loudly rather than
/// mis-execute.
pub(crate) fn call_ret_slot(insn: &Instruction, mode: CpuMode) -> Result<u8, LiftError> {
    // iced exposes the 66h operand-size override on near call/ret as the `w`
    // (16-bit) code forms; those wrap EIP mod 2^16, which we do not model.
    if matches!(
        insn.code(),
        Code::Call_rel16 | Code::Call_rm16 | Code::Retnw | Code::Retnw_imm16
    ) {
        return Err(unsupported_insn(insn));
    }
    Ok(stack_slot(mode))
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

pub(crate) fn unsupported_insn(insn: &Instruction) -> LiftError {
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
pub(crate) fn refill_unsupported_bytes(err: LiftError, code: &[u8], block_start: u64) -> LiftError {
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
