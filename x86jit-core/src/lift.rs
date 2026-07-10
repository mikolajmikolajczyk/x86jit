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
    BtOp, Cond, FPrec, FlagMask, FloatBinOp, FloatUnOp, IrBlock, IrOp, IrRegion, MemOrder,
    PackedBinOp, RegionCaps, RepKind, RmwOp, StrOp, Temp, TempGen, VKLogicOp, VLogicOp, Val,
};
use crate::memory::Memory;
use crate::state::{iced_gpr_index, Reg};

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
            matches!(blk.ops.last(), Some(IrOp::Syscall)),
            "int 0x80 must lift to Syscall, got {:?}",
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
fn lift_insn(
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
        Fld | Fst | Fstp | Fild | Fistp | Fisttp | Fadd | Faddp | Fsub | Fsubp | Fsubr | Fsubrp
        | Fmul | Fmulp | Fdiv | Fdivp | Fdivr | Fdivrp | Fld1 | Fldz | Fabs | Fchs | Fxch
        | Fucomi | Fucomip | Fcomi | Fcomip | Fldcw | Fnstcw | Fnstsw | Fprem => {
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
        // VEX.128 interleave (task-195): 3-operand `dst,a,b` + bits 255:128 cleared.
        // reg_xmm returns None for the VEX.256/ymm forms → those stay deferred.
        Vpunpcklbw => lift_vunpack_avx(insn, ops, 1, false).map(|_| false),
        Vpunpcklwd => lift_vunpack_avx(insn, ops, 2, false).map(|_| false),
        Vpunpckldq => lift_vunpack_avx(insn, ops, 4, false).map(|_| false),
        Vpunpcklqdq => lift_vunpack_avx(insn, ops, 8, false).map(|_| false),
        Vpunpckhbw => lift_vunpack_avx(insn, ops, 1, true).map(|_| false),
        Vpunpckhwd => lift_vunpack_avx(insn, ops, 2, true).map(|_| false),
        Vpunpckhdq => lift_vunpack_avx(insn, ops, 4, true).map(|_| false),
        Vpunpckhqdq => lift_vunpack_avx(insn, ops, 8, true).map(|_| false),
        Packuswb => lift_packuswb(insn, ops).map(|_| false),
        // VEX/EVEX saturating pack `vpack{ss,us}{wb,dw}` (task-195): python3 hits vpackusdw.
        // Register src; any width. VEX upper-zeroing is implicit in the helper's set_vec.
        Vpacksswb => lift_vpack(insn, ops, 2, true).map(|_| false),
        Vpackuswb => lift_vpack(insn, ops, 2, false).map(|_| false),
        Vpackssdw => lift_vpack(insn, ops, 4, true).map(|_| false),
        Vpackusdw => lift_vpack(insn, ops, 4, false).map(|_| false),
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
        // SSE4.1 pmulld: per-lane low 32 bits of the 32×32 product.
        Pmulld => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MulLo32).map(|_| false),
        // SSE4.1 dword min/max (the 16/8-bit forms are SSE2; these reuse the same ops).
        Pminsd => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MinS).map(|_| false),
        Pmaxsd => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MaxS).map(|_| false),
        Pminud => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MinU).map(|_| false),
        Pmaxud => lift_vpacked_bin(insn, ops, tg, 4, PackedBinOp::MaxU).map(|_| false),
        // SSE4.1 variable blend (mask = XMM0 lane MSBs) and imm8 rounding.
        Blendvps => lift_blendv(insn, ops, tg, 4).map(|_| false),
        Blendvpd => lift_blendv(insn, ops, tg, 8).map(|_| false),
        Pblendvb => lift_blendv(insn, ops, tg, 1).map(|_| false),
        // VEX.128 `vpblendw` (task-195): per-word imm8 blend; python3 hits it. Register src.
        Vpblendw => lift_vpblendw(insn, ops).map(|_| false),
        Roundps => lift_round(insn, ops, tg, FPrec::F32, false).map(|_| false),
        Roundpd => lift_round(insn, ops, tg, FPrec::F64, false).map(|_| false),
        Roundss => lift_round(insn, ops, tg, FPrec::F32, true).map(|_| false),
        Roundsd => lift_round(insn, ops, tg, FPrec::F64, true).map(|_| false),
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
        // Dword packed min/max (SSE4.1 VEX + EVEX, task-195): perl/python3 hit vpminud.
        // Width-generic (128/256/512) + masked via lift_vpacked_bin_avx.
        Vpminud => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MinU).map(|_| false),
        Vpmaxud => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MaxU).map(|_| false),
        Vpminsd => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MinS).map(|_| false),
        Vpmaxsd => lift_vpacked_bin_avx(insn, ops, tg, 4, PackedBinOp::MaxS).map(|_| false),
        // Packed absolute value `vpabs{b,w,d,q}` (VEX/EVEX, task-195): any width, masked.
        Vpabsb => lift_vpabs(insn, ops, 1).map(|_| false),
        Vpabsw => lift_vpabs(insn, ops, 2).map(|_| false),
        Vpabsd => lift_vpabs(insn, ops, 4).map(|_| false),
        Vpabsq => lift_vpabs(insn, ops, 8).map(|_| false),
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
            lift_vextract_wide(insn, ops, 1).map(|_| false)
        }
        Vextracti64x4 | Vextractf64x4 | Vextracti32x8 | Vextractf32x8 => {
            lift_vextract_wide(insn, ops, 2).map(|_| false)
        }
        // EVEX cross-lane align (task-168.5.6).
        Valignd => lift_valign(insn, ops, 4).map(|_| false),
        Valignq => lift_valign(insn, ops, 8).map(|_| false),
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
        // Vector-index `vpermq` (VEX.256 / EVEX) — single-source cross-lane permute. The
        // imm8 form is matched above; python3 hits the EVEX-512 vector-index form.
        Vpermq => lift_vperm1(insn, ops, 8).map(|_| false),
        Vpermd => {
            // EVEX-512 or masked → the shared single-source permute; VEX.256 → ymm fast path.
            if reg_zmm(insn, 0).is_some() || evex_is_masked(insn) {
                return lift_vperm1(insn, ops, 4).map(|_| false);
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
        Cmpss => lift_float_cmp_mask(insn, ops, FPrec::F32, true).map(|_| false),
        Cmppd => lift_float_cmp_mask(insn, ops, FPrec::F64, false).map(|_| false),
        Cmpps => lift_float_cmp_mask(insn, ops, FPrec::F32, false).map(|_| false),
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
        Sqrtss => lift_float_unary(insn, ops, FloatUnOp::Sqrt, FPrec::F32, true).map(|_| false),
        Sqrtsd => lift_float_unary(insn, ops, FloatUnOp::Sqrt, FPrec::F64, true).map(|_| false),
        // VEX scalar sqrt (task-195): 3-operand — sqrt(op2 low), upper from op1, 255:128
        // cleared. python3 hits vsqrtsd.
        Vsqrtss => lift_vfloat_unary_scalar(insn, ops, FloatUnOp::Sqrt, FPrec::F32).map(|_| false),
        Vsqrtsd => lift_vfloat_unary_scalar(insn, ops, FloatUnOp::Sqrt, FPrec::F64).map(|_| false),
        // FMA3 `vf[n]m{add,sub}{132,213,231}{ss,sd,ps,pd}` (task-201). python3 numerics.
        Vfmadd132ss => lift_fma(insn, ops, tg, 132, FPrec::F32, true, false, false).map(|_| false),
        Vfmadd132sd => lift_fma(insn, ops, tg, 132, FPrec::F64, true, false, false).map(|_| false),
        Vfmadd132ps => lift_fma(insn, ops, tg, 132, FPrec::F32, false, false, false).map(|_| false),
        Vfmadd132pd => lift_fma(insn, ops, tg, 132, FPrec::F64, false, false, false).map(|_| false),
        Vfmadd213ss => lift_fma(insn, ops, tg, 213, FPrec::F32, true, false, false).map(|_| false),
        Vfmadd213sd => lift_fma(insn, ops, tg, 213, FPrec::F64, true, false, false).map(|_| false),
        Vfmadd213ps => lift_fma(insn, ops, tg, 213, FPrec::F32, false, false, false).map(|_| false),
        Vfmadd213pd => lift_fma(insn, ops, tg, 213, FPrec::F64, false, false, false).map(|_| false),
        Vfmadd231ss => lift_fma(insn, ops, tg, 231, FPrec::F32, true, false, false).map(|_| false),
        Vfmadd231sd => lift_fma(insn, ops, tg, 231, FPrec::F64, true, false, false).map(|_| false),
        Vfmadd231ps => lift_fma(insn, ops, tg, 231, FPrec::F32, false, false, false).map(|_| false),
        Vfmadd231pd => lift_fma(insn, ops, tg, 231, FPrec::F64, false, false, false).map(|_| false),
        Vfmsub132ss => lift_fma(insn, ops, tg, 132, FPrec::F32, true, false, true).map(|_| false),
        Vfmsub132sd => lift_fma(insn, ops, tg, 132, FPrec::F64, true, false, true).map(|_| false),
        Vfmsub132ps => lift_fma(insn, ops, tg, 132, FPrec::F32, false, false, true).map(|_| false),
        Vfmsub132pd => lift_fma(insn, ops, tg, 132, FPrec::F64, false, false, true).map(|_| false),
        Vfmsub213ss => lift_fma(insn, ops, tg, 213, FPrec::F32, true, false, true).map(|_| false),
        Vfmsub213sd => lift_fma(insn, ops, tg, 213, FPrec::F64, true, false, true).map(|_| false),
        Vfmsub213ps => lift_fma(insn, ops, tg, 213, FPrec::F32, false, false, true).map(|_| false),
        Vfmsub213pd => lift_fma(insn, ops, tg, 213, FPrec::F64, false, false, true).map(|_| false),
        Vfmsub231ss => lift_fma(insn, ops, tg, 231, FPrec::F32, true, false, true).map(|_| false),
        Vfmsub231sd => lift_fma(insn, ops, tg, 231, FPrec::F64, true, false, true).map(|_| false),
        Vfmsub231ps => lift_fma(insn, ops, tg, 231, FPrec::F32, false, false, true).map(|_| false),
        Vfmsub231pd => lift_fma(insn, ops, tg, 231, FPrec::F64, false, false, true).map(|_| false),
        Vfnmadd132ss => lift_fma(insn, ops, tg, 132, FPrec::F32, true, true, false).map(|_| false),
        Vfnmadd132sd => lift_fma(insn, ops, tg, 132, FPrec::F64, true, true, false).map(|_| false),
        Vfnmadd132ps => lift_fma(insn, ops, tg, 132, FPrec::F32, false, true, false).map(|_| false),
        Vfnmadd132pd => lift_fma(insn, ops, tg, 132, FPrec::F64, false, true, false).map(|_| false),
        Vfnmadd213ss => lift_fma(insn, ops, tg, 213, FPrec::F32, true, true, false).map(|_| false),
        Vfnmadd213sd => lift_fma(insn, ops, tg, 213, FPrec::F64, true, true, false).map(|_| false),
        Vfnmadd213ps => lift_fma(insn, ops, tg, 213, FPrec::F32, false, true, false).map(|_| false),
        Vfnmadd213pd => lift_fma(insn, ops, tg, 213, FPrec::F64, false, true, false).map(|_| false),
        Vfnmadd231ss => lift_fma(insn, ops, tg, 231, FPrec::F32, true, true, false).map(|_| false),
        Vfnmadd231sd => lift_fma(insn, ops, tg, 231, FPrec::F64, true, true, false).map(|_| false),
        Vfnmadd231ps => lift_fma(insn, ops, tg, 231, FPrec::F32, false, true, false).map(|_| false),
        Vfnmadd231pd => lift_fma(insn, ops, tg, 231, FPrec::F64, false, true, false).map(|_| false),
        Vfnmsub132ss => lift_fma(insn, ops, tg, 132, FPrec::F32, true, true, true).map(|_| false),
        Vfnmsub132sd => lift_fma(insn, ops, tg, 132, FPrec::F64, true, true, true).map(|_| false),
        Vfnmsub132ps => lift_fma(insn, ops, tg, 132, FPrec::F32, false, true, true).map(|_| false),
        Vfnmsub132pd => lift_fma(insn, ops, tg, 132, FPrec::F64, false, true, true).map(|_| false),
        Vfnmsub213ss => lift_fma(insn, ops, tg, 213, FPrec::F32, true, true, true).map(|_| false),
        Vfnmsub213sd => lift_fma(insn, ops, tg, 213, FPrec::F64, true, true, true).map(|_| false),
        Vfnmsub213ps => lift_fma(insn, ops, tg, 213, FPrec::F32, false, true, true).map(|_| false),
        Vfnmsub213pd => lift_fma(insn, ops, tg, 213, FPrec::F64, false, true, true).map(|_| false),
        Vfnmsub231ss => lift_fma(insn, ops, tg, 231, FPrec::F32, true, true, true).map(|_| false),
        Vfnmsub231sd => lift_fma(insn, ops, tg, 231, FPrec::F64, true, true, true).map(|_| false),
        Vfnmsub231ps => lift_fma(insn, ops, tg, 231, FPrec::F32, false, true, true).map(|_| false),
        Vfnmsub231pd => lift_fma(insn, ops, tg, 231, FPrec::F64, false, true, true).map(|_| false),
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
        // leave = mov rsp, rbp; pop rbp.
        Leave => {
            let stk = stack_slot(mode);
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
            ops.push(IrOp::Syscall);
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
                ops.push(IrOp::Syscall);
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

/// `in`/`out` (imm8 or `dx` form) → `IrOp::PortIo`, a trap-out to the embedder
/// (§5.2). Operand layout (iced): `in acc, port` has op0 = accumulator (`al`/`ax`/
/// `eax`), op1 = the port (imm8 or `dx`); `out port, acc` is the mirror. The access
/// width is the accumulator's operand size (1/2/4). For `out` the accumulator value
/// is read here and carried in the exit; for `in` the embedder writes the result
/// back via `complete_port_in`.
fn lift_port_io(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    dir_out: bool,
) -> Result<(), LiftError> {
    let (acc_idx, port_idx) = if dir_out { (1, 0) } else { (0, 1) };
    let size = operand_size(insn, acc_idx);
    if !matches!(size, 1 | 2 | 4) {
        return Err(unsupported_insn(insn));
    }

    // Port is either an 8-bit immediate or `dx` (low 16 bits).
    let port = match insn.op_kind(port_idx) {
        OpKind::Immediate8 => Val::Imm(insn.immediate(port_idx) & 0xffff),
        OpKind::Register => {
            let dx = read_reg(Reg::Rdx, ops, tg);
            alu_none(ops, tg, |dst| IrOp::And {
                dst,
                a: dx,
                b: Val::Imm(0xffff),
                size: 8,
                set_flags: FlagMask::NONE,
            })
        }
        _ => return Err(unsupported_insn(insn)),
    };

    let value = if dir_out {
        read_reg(Reg::Rax, ops, tg)
    } else {
        Val::Imm(0)
    };

    ops.push(IrOp::PortIo {
        port,
        value,
        size,
        dir_out,
    });
    Ok(())
}

/// `push src` — long-mode default operand size is 8. Store BEFORE committing RSP so
/// a faulting store leaves RSP untouched for the retry (§16 pitfall #0).
fn lift_push(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mode: CpuMode,
) -> Result<(), LiftError> {
    let size = push_pop_size(insn, mode);
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
    // Compat32: ESP wraps mod 2^32 before it is used as the store address (a push at
    // ESP < slot must wrap, not carry into the upper half of the backing u64) (§16).
    emit_sp_wrap(ops, new_rsp, mode);
    ops.push(IrOp::Store {
        addr: Val::Temp(new_rsp),
        src,
        size,
        order: MemOrder::None,
    });
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: sp_write_size(mode),
    });
    Ok(())
}

/// `pop dst` — Load BEFORE committing so a faulting load leaves state untouched.
/// `pop rsp` works because the destination write is emitted last and overrides the
/// RSP increment.
fn lift_pop(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    mode: CpuMode,
) -> Result<(), LiftError> {
    let size = push_pop_size(insn, mode);
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
    // A 4-byte RSP write in Compat32 zero-extends → ESP wraps mod 2^32. `pop rsp`
    // works because the destination write is emitted last and overrides this.
    ops.push(IrOp::WriteReg {
        reg: Reg::Rsp,
        src: Val::Temp(new_rsp),
        size: sp_write_size(mode),
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

/// Register index of a vector operand (XMM/YMM/ZMM, 0–31), dropping the width — the
/// register-vs-memory `$ext` for [`vec_src_dispatch!`] on EVEX ops (task-195).
fn vec_operand_reg(insn: &Instruction, op_idx: u32) -> Option<u8> {
    vec_operand(insn, op_idx).map(|(r, _)| r)
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
    // EVEX-512 broadcast from a memory element: load the `elem`-byte scalar and replicate
    // across all 512 bits via the width-generic `VBroadcastGpr` (task-195). glibc's
    // AVX-512 routines broadcast a constant word/dword from `.rodata` (`vpbroadcastw zmm,
    // [rip+k]`). An xmm-source 512-bit broadcast still defers.
    if width == 64 && !evex_is_masked(insn) && insn.op_kind(1) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        let t = tg.fresh();
        ops.push(IrOp::Load {
            dst: t,
            addr,
            size: elem,
        });
        ops.push(IrOp::VBroadcastGpr {
            dst,
            src: Val::Temp(t),
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
    // EVEX masked packed arith `vp{add,sub,min,max,mull}{k}{z}` (task-168.5.5): compute
    // per-lane then merge/zero-mask under `k`. Register src2 only (masked mem-src
    // deferred); any width (128/256/512). glibc's AVX-512 loops mask tail lanes this way.
    if let Some(k) = evex_writemask(insn) {
        let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
        let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VMaskedPacked {
            dst,
            a,
            b,
            op,
            k,
            elem: lane,
            zeroing: insn.zeroing_masking(),
            bytes,
        });
        return Ok(());
    }
    // EVEX 512-bit: width-generic wide packed arith (register or memory src2, task-195).
    // glibc's memcpy-family uses `vpaddq zmm, zmm, [mem]`.
    if let Some(d) = reg_zmm(insn, 0) {
        if evex_is_masked(insn) {
            return Err(unsupported_insn(insn));
        }
        let a = reg_zmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        vec_src_dispatch!(
            insn,
            ops,
            tg,
            reg_zmm,
            2,
            |b| ops.push(IrOp::VPackedWide {
                dst: d,
                a,
                b,
                lane,
                op,
                bytes: 64
            }),
            |addr| ops.push(IrOp::VPackedWideM {
                dst: d,
                a,
                addr,
                lane,
                op,
                bytes: 64
            })
        );
        return Ok(());
    }
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
    // EVEX write-masked move `v{k}{z}, v/[mem]` or `[mem]{k}, v` (task-170.1, 168.5.5):
    // blend under the opmask at `elem` granularity. Reg-reg delegates to `VMaskMov`;
    // a memory operand becomes an element-wise `VMaskLoadMem`/`VMaskStoreMem` (masked-off
    // lanes never touch memory — hardware fault suppression).
    if evex_is_masked(insn) {
        let Some(k) = evex_writemask(insn) else {
            return Err(unsupported_insn(insn));
        };
        match (vec_operand(insn, 0), vec_operand(insn, 1)) {
            // reg, reg → masked register blend.
            (Some((dst, bytes)), Some((src, _))) => {
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
            // reg, [mem] → masked load.
            (Some((dst, bytes)), None) if insn.op_kind(1) == OpKind::Memory => {
                let addr = effective_address(insn, ops, tg)?;
                ops.push(IrOp::VMaskLoadMem {
                    dst,
                    addr,
                    k,
                    elem,
                    zeroing: insn.zeroing_masking(),
                    bytes,
                });
                return Ok(());
            }
            // [mem], reg → masked store (no zeroing form).
            (None, Some((src, bytes))) if insn.op_kind(0) == OpKind::Memory => {
                let addr = effective_address(insn, ops, tg)?;
                ops.push(IrOp::VMaskStoreMem {
                    src,
                    addr,
                    k,
                    elem,
                    bytes,
                });
                return Ok(());
            }
            _ => return Err(unsupported_insn(insn)),
        }
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

/// EVEX lane extract `vextracti{32x4,64x2,32x8,64x4}` (task-195): extract `extract_lanes`
/// 128-bit lanes from `op1` (ZMM/YMM) at the imm8-selected position into `op0` (XMM/YMM
/// register; memory dst deferred). Masking deferred.
fn lift_vextract_wide(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    extract_lanes: u8,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let dst = vec_operand_reg(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (src, src_bytes) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let slots = (src_bytes as u8 / 16) / extract_lanes; // number of extract positions
    let idx = (insn.immediate(2) as u8) & (slots - 1);
    ops.push(IrOp::VExtractLaneWide {
        dst,
        src,
        idx,
        num_lanes: extract_lanes,
    });
    Ok(())
}

/// EVEX lane insert `vinserti{32x4,64x2,64x4}` (task-168.5.6): insert `insert_lanes`
/// 128-bit lanes from `op2` (register; memory deferred) into `op1` at the imm8-selected
/// position, writing `op0`. Masking deferred.
fn lift_vinsert_wide(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    insert_lanes: u8,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (src, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let (ins, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let slots = (bytes as u8 / 16) / insert_lanes; // number of insert positions
    let idx = (insn.immediate(3) as u8) & (slots - 1);
    ops.push(IrOp::VInsertLaneWide {
        dst,
        src,
        ins,
        idx,
        num_lanes: insert_lanes,
        bytes,
    });
    Ok(())
}

/// EVEX `valign{d,q}` (task-168.5.6): shift the `src1:src2` concatenation by an imm8
/// element count. Register src2 only (memory deferred); masking deferred.
fn lift_valign(insn: &Instruction, ops: &mut Vec<IrOp>, elem: u8) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let shift = insn.immediate(3) as u8;
    ops.push(IrOp::VAlign {
        dst,
        a,
        b,
        shift,
        elem,
        bytes,
    });
    Ok(())
}

/// SSE4.1 variable blend `blendvps`/`blendvpd`/`pblendvb` (task-168.5.4). The blend mask
/// is the implicit XMM0; `dst = op0`, blend source `op1` (register or memory).
fn lift_blendv(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VPBlendV { dst, src, lane }),
        |addr| ops.push(IrOp::VPBlendVM { dst, addr, lane })
    );
    Ok(())
}

/// SSE4.1 `round{ps,pd,ss,sd}` (task-168.5.4): round `op1` (register or memory) into
/// `op0` per the imm8 rounding mode.
fn lift_round(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
    scalar: bool,
) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let mode = insn.immediate(2) as u8;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        1,
        |src| ops.push(IrOp::VPRound {
            dst,
            src,
            prec,
            mode,
            scalar
        }),
        |addr| ops.push(IrOp::VPRoundM {
            dst,
            addr,
            prec,
            mode,
            scalar
        })
    );
    Ok(())
}

/// `pcmpistri`/`pcmpestri` (+ VEX) → ECX index + flags (task-168.5.4). Source 2 is a
/// register or, for the memory form (task-195), `[addr]` loaded as a 128-bit value.
fn lift_pcmpstr_idx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    explicit: bool,
) -> Result<(), LiftError> {
    let a = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(2) as u8;
    if let Some(b) = reg_xmm(insn, 1) {
        ops.push(IrOp::VPcmpStr {
            a,
            b,
            imm,
            explicit,
        });
    } else {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VPcmpStrM {
            a,
            addr,
            imm,
            explicit,
        });
    }
    Ok(())
}

/// EVEX scalar `vrndscale{ss,sd}` (task-195). For scale factor M=0 (imm8[7:4]==0) the
/// operation is a 3-operand `round{ss,sd}`: round op2's low element under the imm8[3:0]
/// rounding-control bits, take bits above the element from op1, and clear bits 255:128.
/// Scaled (M≠0) and write-masked forms are deferred.
fn lift_vrndscale(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let imm = insn.immediate(3) as u8;
    if imm >> 4 != 0 {
        return Err(unsupported_insn(insn)); // non-zero scale factor deferred
    }
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    // imm8[3:0] is the same rounding-control encoding `round{ss,sd}` uses.
    let mode = imm & 0x0f;
    // Merge op1's upper bits into dst, then round op2's low element in place.
    if dst != a {
        ops.push(IrOp::VMov { dst, src: a });
    }
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        reg_xmm,
        2,
        |src| ops.push(IrOp::VPRound {
            dst,
            src,
            prec,
            mode,
            scalar: true
        }),
        |addr| ops.push(IrOp::VPRoundM {
            dst,
            addr,
            prec,
            mode,
            scalar: true
        })
    );
    ops.push(IrOp::VZeroUpper { reg: dst }); // EVEX clears bits 255:128
    Ok(())
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

/// VEX-128 `vpmov{z,s}x*` (task-195): the SSE zero/sign-extend plus VEX's upper-zeroing.
/// A YMM destination (256-bit extend) → `reg_xmm` is `None` in `lift_pmovx` → unsupported.
fn lift_vpmovx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: u8,
    to: u8,
    signed: bool,
) -> Result<(), LiftError> {
    // Wide (ymm/zmm) dest — EVEX/VEX-256 — or a masked xmm dest: route to the shared
    // widening helper (register src only; memory-src wide forms deferred). glibc's v4
    // routines use `vpmovsxdq zmm, ymm` to widen dword indices to qword.
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    if dst_width > 16 || evex_is_masked(insn) {
        let src = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VPMovExtendWide {
            dst,
            src,
            from,
            to,
            signed,
            dst_width,
            writemask: evex_writemask(insn),
            zeroing: insn.zeroing_masking(),
        });
        return Ok(());
    }
    // 128-bit dest (SSE4.1 VEX-128): inline extend + VEX upper-zeroing.
    lift_pmovx(insn, ops, tg, from, to, signed)?;
    ops.push(IrOp::VZeroUpper { reg: dst }); // VEX.128 clears bits 255:128
    Ok(())
}

/// Packed absolute value `vpabs{b,w,d,q}` (VEX/EVEX, task-195): `dst = |src|` per
/// `elem`-byte lane, any width, masked/zeroing. Register src only (memory-src deferred);
/// `vec_operand` gives the dest width (= VL), above which EVEX zeroes.
fn lift_vpabs(insn: &Instruction, ops: &mut Vec<IrOp>, elem: u8) -> Result<(), LiftError> {
    let (dst, dst_width) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let src = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPAbs {
        dst,
        src,
        elem,
        dst_width,
        writemask: evex_writemask(insn),
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// AVX512-VPOPCNTDQ `vpopcnt{d,q}` (task-195): per-lane population count over 128/256/512
/// bits, register or memory source. Masked forms are deferred.
fn lift_vpopcnt(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    lane: u8,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        1,
        |a| ops.push(IrOp::VPopcnt {
            dst,
            a,
            lane,
            bytes
        }),
        |addr| ops.push(IrOp::VPopcntM {
            dst,
            addr,
            lane,
            bytes
        })
    );
    Ok(())
}

/// `vpermt2{b,w,d,q}` (task-195): two-table cross-lane permute. iced op order is (dst,
/// idx, tbl); `dst` is also table 0 (its old value). Register src only (memory deferred).
fn lift_vpermt2(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    lift_vperm2(insn, ops, tg, elem, false)
}

/// `vpermi2{b,w,d,q}` (task-195): index-mode two-table permute — the OLD `dst` is the
/// index and `src1`/`src2` are the two tables (t-mode swaps index and table 0).
fn lift_vpermi2(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
) -> Result<(), LiftError> {
    lift_vperm2(insn, ops, tg, elem, true)
}

fn lift_vperm2(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
    imode: bool,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let idx = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let writemask = evex_writemask(insn);
    let zeroing = insn.zeroing_masking();
    // Table 1 (op2) is a register or a memory operand.
    if insn.op_kind(2) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VPermT2M {
            dst,
            idx,
            addr,
            elem,
            writemask,
            zeroing,
            bytes,
            imode,
        });
        return Ok(());
    }
    let tbl = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPermT2 {
        dst,
        idx,
        tbl,
        elem,
        writemask,
        zeroing,
        bytes,
        imode,
    });
    Ok(())
}

/// EVEX narrowing move `vpmov{q,d,w}{d,w,b}` (task-195): truncate each `from`-byte
/// source lane to `to` bytes. `src` (op1) carries the vector width; the destination
/// (op0) must be a register — `vec_operand_reg` returns `None` for the memory-dest form,
/// leaving it deferred.
fn lift_vpmov_narrow(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: u8,
    to: u8,
) -> Result<(), LiftError> {
    let (src, src_width) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    // Memory destination (unmasked): truncate + store contiguously. Masked memory-dest
    // (per-lane fault suppression) is deferred.
    if insn.op_kind(0) == OpKind::Memory {
        if evex_is_masked(insn) {
            return Err(unsupported_insn(insn));
        }
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VPmovNarrowMem {
            src,
            addr,
            from,
            to,
            src_width,
        });
        return Ok(());
    }
    let dst = vec_operand_reg(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPmovNarrow {
        dst,
        src,
        from,
        to,
        src_width,
        writemask: evex_writemask(insn),
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// `kunpck{bw,wd,dq}` (task-195): interleave two opmasks into a wider one — `k[dst] =
/// (k[a]_low << half) | k[b]_low`. iced op order is (dst, src1=a, src2=b).
fn lift_kunpck(insn: &Instruction, ops: &mut Vec<IrOp>, half: u8) -> Result<(), LiftError> {
    let dst = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_kmask(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VKUnpack { dst, a, b, half });
    Ok(())
}

/// Opmask bitwise logic `k{or,and,andn,xor,xnor}{b,w,d,q}` (task-195): `k[dst] =
/// op(k[a], k[b])` over the low `width` bits. iced op order is (dst, src1=a, src2=b).
fn lift_kbinop(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: VKLogicOp,
    width: u8,
) -> Result<(), LiftError> {
    let dst = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_kmask(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VKBinOp {
        dst,
        a,
        b,
        op,
        width,
    });
    Ok(())
}

/// Opmask complement `knot{b,w,d,q}` (task-195): `k[dst] = ~k[a]` over `width` bits.
fn lift_knot(insn: &Instruction, ops: &mut Vec<IrOp>, width: u8) -> Result<(), LiftError> {
    let dst = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VKNot { dst, a, width });
    Ok(())
}

/// Opmask shift `kshift{l,r}{b,w,d,q}` (task-195): `k[dst] = k[a] {<<,>>} imm8` within the
/// low `width` bits. iced op order is (dst, src, imm8).
fn lift_kshift(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    width: u8,
    left: bool,
) -> Result<(), LiftError> {
    let dst = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_kmask(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let amount = insn.immediate(2) as u8;
    ops.push(IrOp::VKShift {
        dst,
        a,
        amount,
        width,
        left,
    });
    Ok(())
}

/// EVEX bitwise logic `vpxor{d,q}` / `vpand{d,q}` / `vpor{d,q}` / `vpandn{d,q}`
/// (task-168.5.2). Width-generic (128/256/512) via [`IrOp::VLogicWide`]; the `d`/`q`
/// suffix only picks the mask granularity, irrelevant unmasked. Register src2 only;
/// masked forms are deferred (they belong with the masked-EVEX-data-op work, 168.5.5).
fn lift_evex_vlogic(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: VLogicOp,
    elem: u8,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    // A write-mask (k1–k7) selects the masked form; k0/none is plain unmasked logic. The
    // `d`/`q` suffix sets the masking granularity (`elem` = 4 or 8 bytes) (task-168.5.5).
    if let Some(k) = evex_writemask(insn) {
        // Masked memory-source logic is deferred; masked reg-src only.
        let (b, _) = vec_operand(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
        ops.push(IrOp::VMaskedLogic {
            dst,
            a,
            b,
            op,
            k,
            elem,
            zeroing: insn.zeroing_masking(),
            bytes,
        });
        return Ok(());
    }
    // Unmasked: register or memory src2 (task-195).
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |b| ops.push(IrOp::VLogicWide {
            dst,
            a,
            b,
            op,
            bytes
        }),
        |addr| ops.push(IrOp::VLogicWideM {
            dst,
            a,
            addr,
            op,
            bytes
        })
    );
    Ok(())
}

/// EVEX `vpternlog{d,q}` (task-168.5.2): 3-input bitwise logic via an 8-bit truth table.
/// `dst` is both the first source and the destination; `src3` register only (memory
/// deferred); masked forms deferred.
fn lift_vpternlog(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn));
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (b, _) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(3) as u8;
    // src3 is a register or a memory vector (task-195).
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |c| ops.push(IrOp::VPTernlog {
            dst,
            b,
            c,
            imm,
            bytes
        }),
        |addr| ops.push(IrOp::VPTernlogM {
            dst,
            b,
            addr,
            imm,
            bytes
        })
    );
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
/// EVEX `vptestm{b,w,d,q}` / `vptestnm{b,w,d,q}` → opmask (task-168.5.4): `k = (a & b)`
/// per-lane test (or its negation for `nm`). Register sources (memory deferred). glibc's
/// AVX-512 `strlen`/`memchr` use `vptestnmb` to locate zero bytes.
fn lift_vptest(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
    neg: bool,
) -> Result<(), LiftError> {
    let k = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, width) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let writemask = evex_writemask(insn);
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |b| ops.push(IrOp::VPTestToMask {
            k,
            a,
            b,
            elem,
            width,
            neg,
            writemask
        }),
        |addr| ops.push(IrOp::VPTestToMaskM {
            k,
            a,
            addr,
            elem,
            width,
            neg,
            writemask
        })
    );
    Ok(())
}

fn lift_vpcmp(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    elem: u8,
    signed: bool,
) -> Result<(), LiftError> {
    let k = reg_kmask(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let (a, width) = vec_operand(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let pred = insn.immediate(3) as u8;
    // EVEX write-mask k1–k7 (k0 = unmasked); vpcmp uses it as a compare predicate.
    let writemask = evex_writemask(insn);
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |b| ops.push(IrOp::VPCmpToMask {
            k,
            a,
            b,
            elem,
            width,
            pred,
            signed,
            writemask
        }),
        |addr| ops.push(IrOp::VPCmpToMaskM {
            k,
            a,
            addr,
            elem,
            width,
            pred,
            signed,
            writemask
        })
    );
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
    let writemask = evex_writemask(insn);
    vec_src_dispatch!(
        insn,
        ops,
        tg,
        vec_operand_reg,
        2,
        |b| ops.push(IrOp::VPCmpToMask {
            k,
            a,
            b,
            elem,
            width,
            pred,
            signed,
            writemask
        }),
        |addr| ops.push(IrOp::VPCmpToMaskM {
            k,
            a,
            addr,
            elem,
            width,
            pred,
            signed,
            writemask
        })
    );
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

/// VEX.128 `vpsrldq`/`vpslldq` (task-195): 3-operand `dst = a shifted by imm8 bytes`,
/// then clear bits 255:128. `reg_xmm` on op0/op1 keeps this VEX.128-only.
fn lift_byteshift_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    right: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let bytes = insn.immediate(2) as u8;
    ops.push(IrOp::VByteShift {
        dst: d,
        a,
        bytes,
        right,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
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

/// VEX.128 `vpshufd xmm, xmm/m, imm8` (task-168): the SSE dword shuffle plus VEX's
/// upper-zeroing. A YMM/EVEX form → `reg_xmm` is `None` in `lift_pshufd` → unsupported
/// (256-bit defers). glibc/coreutils emit the VEX-128 form freely once AVX is on.
/// Single-source cross-lane permute `vperm{d,q}` (vector-index, task-195): register src
/// only (memory src deferred). `vec_operand` gives the width; masked/zeroing supported.
fn lift_vperm1(insn: &Instruction, ops: &mut Vec<IrOp>, elem: u8) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let idx = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let src = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPerm1 {
        dst,
        idx,
        src,
        elem,
        bytes,
        writemask: evex_writemask(insn),
        zeroing: insn.zeroing_masking(),
    });
    Ok(())
}

/// VEX/EVEX `vpack{ss,us}{wb,dw}` (task-195): 3-operand saturating pack, register src2.
/// Any width; the helper's `set_vec` zeroes bits above the register (VEX/EVEX semantics).
fn lift_vpack(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    from_elem: u8,
    signed: bool,
) -> Result<(), LiftError> {
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VPackWide {
        dst,
        a,
        b,
        from_elem,
        signed,
        bytes,
    });
    Ok(())
}

/// VEX.128 `vpblendw` (task-195): 3-operand per-word imm8 blend + upper-zeroing. Register
/// src2 only (memory src deferred).
fn lift_vpblendw(insn: &Instruction, ops: &mut Vec<IrOp>) -> Result<(), LiftError> {
    let dst = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    let imm = insn.immediate(3) as u8;
    ops.push(IrOp::VBlendW { dst, a, b, imm });
    ops.push(IrOp::VZeroUpper { reg: dst }); // VEX.128 clears bits 255:128
    Ok(())
}

fn lift_vpshufd(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
) -> Result<(), LiftError> {
    // Wide (ymm/zmm) or masked → shared per-lane helper (register src only; memory src
    // for the wide form is deferred). python3 hits `vpshufd ymm`.
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    if bytes > 16 || evex_is_masked(insn) {
        let a = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        let imm = insn.immediate(2) as u8;
        ops.push(IrOp::VShuffle32Wide {
            dst,
            a,
            imm,
            bytes,
            writemask: evex_writemask(insn),
            zeroing: insn.zeroing_masking(),
        });
        return Ok(());
    }
    lift_pshufd(insn, ops, tg)?;
    ops.push(IrOp::VZeroUpper { reg: dst }); // VEX.128 clears bits 255:128
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

/// VEX.128 `vpunpck{l,h}{bw,wd,dq,qdq}` (task-195): 3-operand interleave `dst =
/// unpack(a, b)` then clear bits 255:128. Register src2 only — `reg_xmm` returns
/// `None` for the VEX.256/ymm forms (per-128-lane semantics), leaving them deferred.
fn lift_vunpack_avx(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    lane: u8,
    high: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    ops.push(IrOp::VUnpackLow {
        dst: d,
        a,
        b,
        lane,
        high,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
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

/// VEX `vmov{l,h}p{s,d}` (task-195). Two shapes: the store `[mem], xmm` (operand-identical
/// to SSE) and the 3-operand load `xmm, xmm, m64` (bits from the merge source `op1`, the
/// half loaded from `op2`, VEX zeroing bits 255:128).
fn lift_vhalf_mem(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    high: bool,
) -> Result<(), LiftError> {
    // Store form: `[mem], xmm` — reuse the SSE lowering (no upper-zeroing on a store).
    if insn.op_kind(0) == OpKind::Memory {
        return lift_half_mem(insn, ops, tg, high);
    }
    // Load form: `xmm, xmm(merge), m64`.
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    if insn.op_kind(2) != OpKind::Memory {
        return Err(unsupported_insn(insn));
    }
    let addr = effective_address(insn, ops, tg)?;
    if d != a {
        ops.push(IrOp::VMov { dst: d, src: a });
    }
    ops.push(IrOp::VLoadHalf { dst: d, addr, high });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
    Ok(())
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

/// VEX `vmovs{s,d}` (task-195). Three shapes: store `[mem], xmm`; 2-operand load `xmm,
/// m` (dst = the loaded scalar, all upper bits zeroed); and 3-operand register merge
/// `xmm, xmm, xmm` (low element from `op2`, bits 127:64 from `op1`, bits 255:128 zeroed).
fn lift_vscalar_fmove(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
) -> Result<(), LiftError> {
    let size = prec.bytes();
    // Store form: `[mem], xmm` — plain scalar store, no register write.
    if insn.op_kind(0) == OpKind::Memory {
        let s = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VStore { addr, src: s, size });
        return Ok(());
    }
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    // 2-operand memory load: the scalar goes to the low element, all upper bits zero.
    if insn.op_kind(1) == OpKind::Memory {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VLoad { dst: d, addr, size });
        ops.push(IrOp::VZeroUpper { reg: d }); // VEX zeroes bits 255:128 (VLoad zeroes 127:size)
        return Ok(());
    }
    // 3-operand register merge: bits 127:64 from `op1`, low element from `op2`.
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let b = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    if d != a {
        ops.push(IrOp::VMov { dst: d, src: a });
    }
    ops.push(IrOp::VFloatMov {
        dst: d,
        src: b,
        prec,
    });
    ops.push(IrOp::VZeroUpper { reg: d });
    Ok(())
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

/// VEX `v{add,sub,mul,div,min,max}{ss,sd,ps,pd}` 128-bit (task-195): 3-operand `dst =
/// op(op1, op2)`. Pre-copy `op1` into `dst` so the SSE `VFloatBin`/`VFloatBinM` lowering
/// (which treats the destination as the first source and, for a scalar op, keeps bits
/// 127:64) sees the right merge base; then VEX zeroes bits 255:128. `op2` may be memory.
/// A YMM operand → `reg_xmm` is `None` → unsupported (256-bit defers).
fn lift_vfloat_bin(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    op: FloatBinOp,
    prec: FPrec,
    scalar: bool,
) -> Result<(), LiftError> {
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
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
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

/// `cvt{,u}si2s*`: integer (gpr/mem) → float in the destination's low lane. `signed`
/// picks the signed `cvtsi2s*` vs the AVX-512 unsigned `cvtusi2s*` form (task-195).
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
        signed: true,
    });
    Ok(())
}

/// VEX/EVEX `vcvt{,u}si2s{s,d} xmm, xmm, r/m` (task-195): 3-operand int→scalar-float. The
/// result's bits 127:64 come from `op1` (the merge source), the low element is the
/// converted integer at `op2`, and the upper bits above 128 are zeroed. Copy `op1` into
/// `dst` first so `VCvtFromInt` (which preserves the upper bits) leaves the right merge.
fn lift_vcvt_from_int(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
    signed: bool,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let int_size = operand_size(insn, 2);
    let src = lower_read(insn, 2, ops, tg)?;
    if d != a {
        ops.push(IrOp::VMov { dst: d, src: a });
    }
    ops.push(IrOp::VCvtFromInt {
        dst: d,
        src,
        int_size,
        prec,
        signed,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX/EVEX clears bits 255:128
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
    lift_cvt_to_int_signed(insn, ops, tg, prec, trunc, true)
}

/// `cvt(t)s*2usi` (AVX-512, task-195): float → **unsigned** integer in a GPR. Same shape
/// as the signed `*2si` form; `signed = false` picks the unsigned saturating cast.
fn lift_cvt_to_int_signed(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    prec: FPrec,
    trunc: bool,
    signed: bool,
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
        signed,
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

/// FMA3 `vf[n]m{add,sub}{132,213,231}{ss,sd,ps,pd}` (task-201): resolve the 132/213/231
/// operand order into `x`/`y`/`z` roles (op0=dst, op1=vvvv, op2=reg/mem), then emit a
/// fused multiply-add. `neg_prod`/`neg_add` pick the sign. Masked EVEX forms are deferred.
#[allow(clippy::too_many_arguments)]
fn lift_fma(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    order: u16,
    prec: FPrec,
    scalar: bool,
    neg_prod: bool,
    neg_add: bool,
) -> Result<(), LiftError> {
    if evex_is_masked(insn) {
        return Err(unsupported_insn(insn)); // masked EVEX FMA deferred
    }
    let (dst, bytes) = vec_operand(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let op1 = vec_operand_reg(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let mem = insn.op_kind(2) == OpKind::Memory;
    let op2 = if mem {
        0
    } else {
        vec_operand_reg(insn, 2).ok_or_else(|| unsupported_insn(insn))?
    };
    // op0=dst, op1, op2. 132: dst*op2+op1; 213: op1*dst+op2; 231: op1*op2+dst. The memory
    // operand is always op2 → it lands in y (132/231) or z (213).
    let (x, y, z, mem_role) = match order {
        132 => (dst, op2, op1, 1u8),
        213 => (op1, dst, op2, 2u8),
        _ => (op1, op2, dst, 1u8), // 231
    };
    if mem {
        let addr = effective_address(insn, ops, tg)?;
        ops.push(IrOp::VFmaM {
            dst,
            x,
            y,
            z,
            addr,
            mem_role,
            prec,
            scalar,
            neg_prod,
            neg_add,
            bytes,
        });
    } else {
        ops.push(IrOp::VFma {
            dst,
            x,
            y,
            z,
            prec,
            scalar,
            neg_prod,
            neg_add,
            bytes,
        });
    }
    Ok(())
}

/// VEX scalar float-unary `vsqrt{ss,sd}` (task-195): 3-operand — the low element is
/// `op(op2)`, bits above it come from op1, and bits 255:128 are cleared. Register src2.
fn lift_vfloat_unary_scalar(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    op: FloatUnOp,
    prec: FPrec,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let s = reg_xmm(insn, 2).ok_or_else(|| unsupported_insn(insn))?;
    // Merge op1's upper bits into dst, then apply the unary to op2's low element.
    if d != a {
        ops.push(IrOp::VMov { dst: d, src: a });
    }
    ops.push(IrOp::VFloatUnary {
        dst: d,
        src: s,
        op,
        prec,
        scalar: true,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
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

/// VEX scalar `vcvtss2sd`/`vcvtsd2ss` (task-195): 3-operand — bits above the low
/// element come from `op1`, the converted low element from `op2`, bits 255:128 cleared.
/// Register or memory op2; `reg_xmm` on op0/op1 keeps this VEX.128-only.
fn lift_vcvt_scalar(
    insn: &Instruction,
    ops: &mut Vec<IrOp>,
    tg: &mut TempGen,
    from: FPrec,
    to: FPrec,
) -> Result<(), LiftError> {
    let d = reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?;
    let a = reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?;
    let src = read_scalar_float(insn, 2, ops, tg, from)?;
    // Merge op1's upper bits into dst, then overwrite the low element via the convert
    // (VCvtFloat preserves dst[127:size]). Order matters when d == a: the VMov is a no-op.
    if d != a {
        ops.push(IrOp::VMov { dst: d, src: a });
    }
    ops.push(IrOp::VCvtFloat {
        dst: d,
        src,
        from,
        to,
    });
    ops.push(IrOp::VZeroUpper { reg: d }); // VEX.128 clears bits 255:128
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
        Fisttp => emit(
            match msz {
                2 => K::FisttpI16,
                8 => K::FisttpI64,
                _ => K::FisttpI32,
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
    mode: CpuMode,
) -> Result<Val, LiftError> {
    match insn.op_kind(0) {
        // Direct near branch: iced resolves the target already truncated to the
        // decode bitness, so a 32-bit decode yields a target < 2^32 (mask is a no-op
        // but pins the invariant); no runtime op needed.
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
            Ok(Val::Imm(mask_pc(insn.near_branch_target(), mode)))
        }
        // Indirect near branch: the loaded target must wrap mod 2^32 in Compat32.
        _ => {
            let target = lower_read(insn, 0, ops, tg)?;
            if mode.wraps_32() {
                let masked = tg.fresh();
                ops.push(IrOp::And {
                    dst: masked,
                    a: target,
                    b: Val::Imm(0xFFFF_FFFF),
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

/// push/pop transfer size. iced already reflects the effective operand size in the
/// operand width: long-mode default 8 (66h → 2), Compat32 default 4 (66h → 2). The
/// zero fallback (an implicit-operand form with no width) uses the mode default.
fn push_pop_size(insn: &Instruction, mode: CpuMode) -> u8 {
    let s = operand_size(insn, 0);
    if s == 0 {
        stack_slot(mode)
    } else {
        s
    }
}

/// Default stack-frame width for the mode: 8 in long mode, 4 in Compat32.
fn stack_slot(mode: CpuMode) -> u8 {
    if mode.wraps_32() {
        4
    } else {
        8
    }
}

/// Width of a full-width RSP/ESP write: 8 in long mode (leaves RSP intact), 4 in
/// Compat32 (zero-extends → ESP wraps mod 2^32 via the central GPR write path).
fn sp_write_size(mode: CpuMode) -> u8 {
    stack_slot(mode)
}

/// Truncate a computed PC/return address to the mode's pointer width (Compat32: mod
/// 2^32). Long mode passes through. Used for direct-branch targets and return
/// addresses, which are `Val::Imm` literals resolved at lift time.
fn mask_pc(addr: u64, mode: CpuMode) -> u64 {
    if mode.wraps_32() {
        addr & 0xFFFF_FFFF
    } else {
        addr
    }
}

/// Emit a mod-2^32 mask on a freshly-computed stack pointer temp (Compat32 only), so
/// it is a valid 32-bit address before it is used as a store address (§16).
fn emit_sp_wrap(ops: &mut Vec<IrOp>, sp: Temp, mode: CpuMode) {
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
fn call_ret_slot(insn: &Instruction, mode: CpuMode) -> Result<u8, LiftError> {
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
