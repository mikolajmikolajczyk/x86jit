//! Extracted `interpret_block` dispatch arm bodies (control); see `super`.

use super::*;
use crate::ir::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_x87(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    kind: &crate::x87::FpuKind,
    addr: &Val,
    sti: &u8,
) -> Option<StepResult> {
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
        return Some(StepResult::Exit(Exit::UnmappedMemory {
            addr: fault,
            access,
        }));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_fx_state(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    addr: &Val,
    restore: &bool,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    // Through `Memory` (RAM check + SMC note_write), like the x87 arm.
    if let Some((fault, write)) = crate::x87::exec_fxstate(cpu, mem, a, *restore) {
        cpu.rip = cur_addr;
        return Some(StepResult::Exit(Exit::UnmappedMemory {
            addr: fault,
            access: if write {
                AccessKind::Write
            } else {
                AccessKind::Read
            },
        }));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_set_df(cpu: &mut CpuState, value: &bool) -> Option<StepResult> {
    cpu.flags.df = *value;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_rep_string(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &[u64],
    cur_addr: u64,
    op: &StrOp,
    elem: &u8,
    rep: &RepKind,
    addr_bits: &u8,
    seg_base: &Val,
) -> Option<StepResult> {
    // Resolve the DS-source segment base (0, or the FS/GS base under an override).
    let seg_base = read_val(*seg_base, temps);
    // Route every element through `Memory` (region check + SMC `note_write`),
    // exactly like a scalar `Store` — so `rep stos` onto a code page is
    // caught and an MMIO/unmapped target traps (§10).
    if let Some(f) = string_run(cpu, mem, *op, *elem, *rep, cur_addr, *addr_bits, seg_base) {
        // `string_run` already set RIP to the faulting instruction.
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

/// Real-mode (§17.6) string op: like [`exec_rep_string`] but with both segment bases
/// resolved (`ds<<4` source, `es<<4` dest, already computed into the temps). Address
/// size is always 16-bit. Routes through `string_run_impl` so the ES-destination base is
/// honoured (the long/compat `string_run` hardcodes it to 0).
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_rep_string_real(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &[u64],
    cur_addr: u64,
    op: &StrOp,
    elem: &u8,
    rep: &RepKind,
    ds_base: &Val,
    es_base: &Val,
) -> Option<StepResult> {
    let ds_base = read_val(*ds_base, temps);
    let es_base = read_val(*es_base, temps);
    if let Some(f) = string_run_impl(cpu, mem, *op, *elem, *rep, cur_addr, 16, ds_base, es_base) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

/// `lahf` (§17.6): AH = the low byte of the real-mode FLAGS image (SF ZF 0 AF 0 PF 1 CF).
pub(crate) fn exec_lahf(cpu: &mut CpuState) -> Option<StepResult> {
    let ah = (cpu.flags.to_flags16() & 0xFF) as u64;
    cpu.gpr[RAX] = (cpu.gpr[RAX] & !0xFF00) | (ah << 8);
    None
}

/// `sahf` (§17.6): set SF/ZF/AF/PF/CF from AH bits 7/6/4/2/0. OF, DF and IF are untouched.
pub(crate) fn exec_sahf(cpu: &mut CpuState) -> Option<StepResult> {
    let ah = (cpu.gpr[RAX] >> 8) as u8;
    cpu.flags.cf = ah & (1 << 0) != 0;
    cpu.flags.set_pf(ah & (1 << 2) != 0);
    cpu.flags.set_af(ah & (1 << 4) != 0);
    cpu.flags.zf = ah & (1 << 6) != 0;
    cpu.flags.sf = ah & (1 << 7) != 0;
    None
}

/// `pusha` (§17.6): push AX, CX, DX, BX, the *original* SP, BP, SI, DI onto SS:SP
/// (16-bit wraps; AX ends at the highest address, DI at the lowest). A store fault traps
/// out (RIP left on the instruction; a partial-frame SP change is possible mid-push, as
/// in IVT delivery).
pub(crate) fn exec_pusha_real(
    cpu: &mut CpuState,
    mem: &Memory,
    cur_addr: u64,
) -> Option<StepResult> {
    let orig_sp = cpu.gpr[RSP] as u16;
    let words = [
        cpu.gpr[RAX] as u16,
        cpu.gpr[RCX] as u16,
        cpu.gpr[RDX] as u16,
        cpu.gpr[RBX] as u16,
        orig_sp,
        cpu.gpr[RBP] as u16,
        cpu.gpr[RSI] as u16,
        cpu.gpr[RDI] as u16,
    ];
    for w in words {
        if let Err(e) = push16(cpu, mem, cur_addr, w) {
            return Some(e);
        }
    }
    None
}

/// `popa` (§17.6): pop DI, SI, BP, (discard the saved SP word), BX, DX, CX, AX off SS:SP
/// (16-bit wraps, the inverse order of `pusha`). Each 16-bit register write preserves the
/// upper GPR bits. A load fault traps out (RIP left on the instruction).
pub(crate) fn exec_popa_real(
    cpu: &mut CpuState,
    mem: &Memory,
    cur_addr: u64,
) -> Option<StepResult> {
    // Reverse of `pusha`: DI, SI, BP, [SP slot — read and discarded], BX, DX, CX, AX.
    // `None` marks the saved-SP slot: the word is read (advancing SP) but not written back.
    let order = [
        Some(RDI),
        Some(RSI),
        Some(RBP),
        None,
        Some(RBX),
        Some(RDX),
        Some(RCX),
        Some(RAX),
    ];
    for slot in order {
        let v = match pop16(cpu, mem, cur_addr) {
            Ok(v) => v,
            Err(e) => return Some(e),
        };
        if let Some(i) = slot {
            cpu.gpr[i] = (cpu.gpr[i] & !0xFFFF) | v as u64;
        }
    }
    None
}

/// `enter alloc, level` (§17.6): build a nested stack frame. Push BP; copy `level & 0x1F`
/// saved frame pointers (the display) from the enclosing frames; push the new frame
/// pointer; set BP to it; then subtract `alloc` from SP. All SS-relative with 16-bit
/// wraps. A store or display-read fault traps out (RIP left on the instruction).
pub(crate) fn exec_enter_real(
    cpu: &mut CpuState,
    mem: &Memory,
    cur_addr: u64,
    alloc: u16,
    level: u8,
) -> Option<StepResult> {
    let level = level & 0x1F;
    // PUSH BP.
    if let Err(e) = push16(cpu, mem, cur_addr, cpu.gpr[RBP] as u16) {
        return Some(e);
    }
    // frame_ptr = SP after the first push (the new BP).
    let frame_ptr = cpu.gpr[RSP] as u16;
    if level > 0 {
        // Copy the display: for each enclosing level, step BP down by 2 and re-push the
        // frame pointer stored there.
        let mut bp = cpu.gpr[RBP] as u16;
        for _ in 1..level {
            bp = bp.wrapping_sub(2);
            cpu.gpr[RBP] = (cpu.gpr[RBP] & !0xFFFF) | bp as u64;
            let w = match mem.read(ss_addr(cpu, bp), 2) {
                Ok(v) => v as u16,
                Err(t) => {
                    return Some(trap_out(
                        cpu,
                        cur_addr,
                        t,
                        ss_addr(cpu, bp),
                        2,
                        AccessKind::Read,
                        0,
                    ))
                }
            };
            if let Err(e) = push16(cpu, mem, cur_addr, w) {
                return Some(e);
            }
        }
        // Push the new frame pointer itself.
        if let Err(e) = push16(cpu, mem, cur_addr, frame_ptr) {
            return Some(e);
        }
    }
    // BP = frame_ptr; SP -= alloc (16-bit wrap).
    cpu.gpr[RBP] = (cpu.gpr[RBP] & !0xFFFF) | frame_ptr as u64;
    let new_sp = (cpu.gpr[RSP] as u16).wrapping_sub(alloc);
    cpu.gpr[RSP] = (cpu.gpr[RSP] & !0xFFFF) | new_sp as u64;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_jump(cpu: &mut CpuState, temps: &mut [u64], target: &Val) -> Option<StepResult> {
    cpu.rip = read_val(*target, &*temps);
    Some(StepResult::Continue)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_branch(
    cpu: &mut CpuState,
    cond: &Cond,
    taken: &u64,
    fallthrough: &u64,
) -> Option<StepResult> {
    cpu.rip = if eval_cond(*cond, &cpu.flags) {
        *taken
    } else {
        *fallthrough
    };
    Some(StepResult::Continue)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_call(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    target: &Val,
    return_addr: &u64,
    slot: &u8,
    wrap_sp: &bool,
) -> Option<StepResult> {
    let mut sp = cpu.gpr[RSP].wrapping_sub(*slot as u64);
    if *wrap_sp {
        sp &= 0xFFFF_FFFF;
    }
    if let Err(t) = mem.write(sp, *return_addr, *slot) {
        return Some(trap_out(
            cpu,
            cur_addr,
            t,
            sp,
            *slot,
            AccessKind::Write,
            *return_addr,
        ));
    }
    cpu.gpr[RSP] = sp;
    cpu.rip = read_val(*target, &*temps);
    Some(StepResult::Continue)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_ret(
    cpu: &mut CpuState,
    mem: &Memory,
    cur_addr: u64,
    slot: &u8,
    pop_extra: &u16,
    wrap_sp: &bool,
) -> Option<StepResult> {
    let sp = cpu.gpr[RSP];
    match mem.read(sp, *slot) {
        Ok(ret) => {
            let mut nsp = sp
                .wrapping_add(*slot as u64)
                .wrapping_add(*pop_extra as u64);
            if *wrap_sp {
                nsp &= 0xFFFF_FFFF;
            }
            cpu.gpr[RSP] = nsp;
            cpu.rip = ret;
        }
        Err(t) => return Some(trap_out(cpu, cur_addr, t, sp, *slot, AccessKind::Read, 0)),
    }
    Some(StepResult::Continue)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_syscall(
    cpu: &mut CpuState,
    block_end: u64,
    is_amd64: bool,
) -> Option<StepResult> {
    // The AMD64 `syscall` instruction latches RCX <- next-instruction RIP and
    // R11 <- RFLAGS (hardware). The i386 `int 0x80` gate must NOT — its ABI passes
    // args in ECX/… (see `IrOp::Syscall`). The JIT's `emit_syscall` mirrors this.
    if is_amd64 {
        cpu.gpr[RCX] = block_end;
        cpu.gpr[R11] = cpu.flags.to_rflags();
    }
    cpu.rip = block_end;
    Some(StepResult::Exit(Exit::Syscall))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_port_io(
    cpu: &mut CpuState,
    temps: &mut [u64],
    block_end: u64,
    port: &Val,
    value: &Val,
    size: &u8,
    dir_out: &bool,
) -> Option<StepResult> {
    let port = read_val(*port, &*temps) as u16;
    let value = read_val(*value, &*temps) & mask(*size);
    // RIP past the instruction (like `Syscall`): the embedder services the
    // port and re-enters. For `in`, `complete_port_in` will merge the
    // result into the accumulator, so record the pending width.
    cpu.rip = block_end;
    let dir = if *dir_out {
        PortDir::Out
    } else {
        cpu.pending_port_in = Some(*size);
        PortDir::In
    };
    Some(StepResult::Exit(Exit::PortIo {
        port,
        size: *size,
        dir,
        value,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_hlt(cpu: &mut CpuState, block_end: u64) -> Option<StepResult> {
    cpu.rip = block_end;
    Some(StepResult::Exit(Exit::Hlt))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_trap(
    cpu: &mut CpuState,
    cur_addr: u64,
    vector: &u8,
    advance: &u8,
) -> Option<StepResult> {
    // x86 saved-RIP: a fault (advance 0) leaves RIP on the instruction, a
    // trap (advance = length) resumes past it. `addr` mirrors that RIP.
    cpu.rip = cur_addr + *advance as u64;
    Some(StepResult::Exit(Exit::Exception {
        addr: cpu.rip,
        vector: *vector,
    }))
}

// --- real-mode interrupt-flag + IVT delivery (§17.6, sub-seam b) ---

/// The physical stack address for real-mode SS:SP (`ss_base + (sp & 0xFFFF)`).
#[inline]
fn ss_addr(cpu: &CpuState, sp: u16) -> u64 {
    ((cpu.ss as u64) << 4) + sp as u64
}

/// Push a 16-bit word onto the real-mode stack SS:SP: `SP -= 2` (16-bit wrap), then
/// store at SS:SP. Returns the trap on a faulting store (RIP left on `cur_addr`), or
/// `Ok(())` with SP committed. Used by `int`/`pushf`/IVT delivery.
fn push16(cpu: &mut CpuState, mem: &Memory, cur_addr: u64, word: u16) -> Result<(), StepResult> {
    let sp = (cpu.gpr[RSP] as u16).wrapping_sub(2);
    let addr = ss_addr(cpu, sp);
    if let Err(t) = mem.write(addr, word as u64, 2) {
        return Err(trap_out(
            cpu,
            cur_addr,
            t,
            addr,
            2,
            AccessKind::Write,
            word as u64,
        ));
    }
    // Commit SP (16-bit write: preserve the upper GPR bits).
    cpu.gpr[RSP] = (cpu.gpr[RSP] & !0xFFFF) | sp as u64;
    Ok(())
}

/// Pop a 16-bit word off the real-mode stack SS:SP: load at SS:SP, then `SP += 2`
/// (16-bit wrap). Returns the trap on a faulting load (RIP left on `cur_addr`, SP
/// un-advanced), or the popped word with SP committed. Used by `popf`/`iret`.
fn pop16(cpu: &mut CpuState, mem: &Memory, cur_addr: u64) -> Result<u16, StepResult> {
    let sp = cpu.gpr[RSP] as u16;
    let addr = ss_addr(cpu, sp);
    match mem.read(addr, 2) {
        Ok(v) => {
            let nsp = sp.wrapping_add(2);
            cpu.gpr[RSP] = (cpu.gpr[RSP] & !0xFFFF) | nsp as u64;
            Ok(v as u16)
        }
        Err(t) => Err(trap_out(cpu, cur_addr, t, addr, 2, AccessKind::Read, 0)),
    }
}

/// `pushf` (16-bit, §17.6): push the real-mode FLAGS image (incl. IF) onto SS:SP.
pub(crate) fn exec_pushf_real(
    cpu: &mut CpuState,
    mem: &Memory,
    cur_addr: u64,
) -> Option<StepResult> {
    let image = cpu.flags.to_flags16();
    if let Err(e) = push16(cpu, mem, cur_addr, image) {
        return Some(e);
    }
    None
}

/// `popf` (16-bit, §17.6): pop a FLAGS image off SS:SP and restore the modeled flags
/// incl. IF.
pub(crate) fn exec_popf_real(
    cpu: &mut CpuState,
    mem: &Memory,
    cur_addr: u64,
) -> Option<StepResult> {
    match pop16(cpu, mem, cur_addr) {
        Ok(image) => {
            cpu.flags.set_flags16(image);
            None
        }
        Err(e) => Some(e),
    }
}

/// Deliver a software interrupt / in-guest exception through the real-mode IVT (§17.6).
///
/// Pushes FLAGS(2), CS(2), `saved_ip`(2) onto SS:SP (16-bit wraps), clears IF+TF, then
/// loads CS:IP from the IVT (new IP = word at physical `vector*4`, new CS = word at
/// `vector*4 + 2`). The pushes are ordered high-to-low (FLAGS, then CS, then IP) so the
/// popped order on `iret` is IP, CS, FLAGS — matching hardware. A store fault or an
/// unmapped IVT word traps out (RIP left on the faulting instruction for a retry);
/// partial SP damage is possible on a mid-push fault, but a mapped stack (the normal
/// case) never hits it. Ends the block (returns a terminating `StepResult`).
pub(crate) fn deliver_interrupt(
    cpu: &mut CpuState,
    mem: &Memory,
    cur_addr: u64,
    vector: u8,
    saved_ip: u64,
) -> StepResult {
    let flags = cpu.flags.to_flags16();
    // Frame: FLAGS, CS, IP (pushed in that order → highest address is FLAGS).
    if let Err(e) = push16(cpu, mem, cur_addr, flags) {
        return e;
    }
    if let Err(e) = push16(cpu, mem, cur_addr, cpu.cs) {
        return e;
    }
    if let Err(e) = push16(cpu, mem, cur_addr, saved_ip as u16) {
        return e;
    }
    // IF and TF are cleared on entry (TF is a no-op — not modeled, see `Flags`).
    cpu.flags.if_ = false;
    // Vector through the IVT: IP = [vector*4], CS = [vector*4 + 2].
    let ivt = vector as u64 * 4;
    let new_ip = match mem.read(ivt, 2) {
        Ok(v) => v as u16,
        Err(t) => return trap_out(cpu, cur_addr, t, ivt, 2, AccessKind::Read, 0),
    };
    let new_cs = match mem.read(ivt + 2, 2) {
        Ok(v) => v as u16,
        Err(t) => return trap_out(cpu, cur_addr, t, ivt + 2, 2, AccessKind::Read, 0),
    };
    cpu.cs = new_cs;
    cpu.rip = new_ip as u64;
    StepResult::Continue
}

/// `iret` (16-bit real mode, §17.6): pop IP, CS, FLAGS off SS:SP (16-bit wraps),
/// restoring IF etc., then resume at CS:IP. The pop order is the inverse of the
/// `deliver_interrupt` push order. Ends the block.
pub(crate) fn exec_iret_real(cpu: &mut CpuState, mem: &Memory, cur_addr: u64) -> StepResult {
    let ip = match pop16(cpu, mem, cur_addr) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let cs = match pop16(cpu, mem, cur_addr) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let flags = match pop16(cpu, mem, cur_addr) {
        Ok(v) => v,
        Err(e) => return e,
    };
    cpu.flags.set_flags16(flags);
    cpu.cs = cs;
    cpu.rip = ip as u64;
    StepResult::Continue
}

/// `loop`/`loope`/`loopne`/`jcxz` (§17.6): a CX-driven near branch. `loop*` predecrement
/// CX (16-bit, preserving the upper GPR bits) and branch on CX != 0 — additionally
/// gated by ZF for `loope` (ZF set) / `loopne` (ZF clear). `jcxz` branches on CX == 0
/// without touching CX. Both targets are already 16-bit IP offsets. Ends the block.
pub(crate) fn exec_loop_cx(
    cpu: &mut CpuState,
    kind: &LoopKind,
    taken: &u64,
    fallthrough: &u64,
) -> StepResult {
    let take = match kind {
        LoopKind::Jcxz => (cpu.gpr[RCX] as u16) == 0,
        _ => {
            let cx = (cpu.gpr[RCX] as u16).wrapping_sub(1);
            cpu.gpr[RCX] = (cpu.gpr[RCX] & !0xFFFF) | cx as u64;
            match kind {
                LoopKind::Loop => cx != 0,
                LoopKind::Loope => cx != 0 && cpu.flags.zf,
                LoopKind::Loopne => cx != 0 && !cpu.flags.zf,
                LoopKind::Jcxz => unreachable!(),
            }
        }
    };
    cpu.rip = if take { *taken } else { *fallthrough };
    StepResult::Continue
}

/// Far (inter-segment) `jmp` (§17.6): load CS:IP (both 16-bit-masked). For an `m16:16`
/// operand the CS/IP `Val`s are `Temp`s the preceding `Load` ops filled; a faulting
/// load already trapped before this op runs. Ends the block — the dispatcher recomputes
/// the fetch address from the new CS:IP.
pub(crate) fn exec_far_jump(cpu: &mut CpuState, temps: &[u64], cs: &Val, ip: &Val) -> StepResult {
    cpu.cs = (read_val(*cs, temps) & 0xFFFF) as u16;
    cpu.rip = read_val(*ip, temps) & 0xFFFF;
    StepResult::Continue
}

/// Far `call` (§17.6): push the current CS then the 16-bit return IP onto SS:SP (16-bit
/// wraps, CS at the higher address so `retf` pops IP then CS), then load the target
/// CS:IP. May trap on a stack store (RIP left on the instruction; a partial-frame SP
/// change is possible mid-push, as in IVT delivery). Ends the block.
pub(crate) fn exec_far_call(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &[u64],
    cur_addr: u64,
    cs: &Val,
    ip: &Val,
    ret_ip: &u16,
) -> StepResult {
    let target_cs = (read_val(*cs, temps) & 0xFFFF) as u16;
    let target_ip = read_val(*ip, temps) & 0xFFFF;
    if let Err(e) = push16(cpu, mem, cur_addr, cpu.cs) {
        return e;
    }
    if let Err(e) = push16(cpu, mem, cur_addr, *ret_ip) {
        return e;
    }
    cpu.cs = target_cs;
    cpu.rip = target_ip;
    StepResult::Continue
}

/// Far `ret` / `retf` (§17.6): pop IP then CS off SS:SP (16-bit wraps), then add
/// `pop_extra` to SP (`retf imm16` caller cleanup, 16-bit wrap). May trap on a stack
/// load. Ends the block.
pub(crate) fn exec_far_ret(
    cpu: &mut CpuState,
    mem: &Memory,
    cur_addr: u64,
    pop_extra: &u16,
) -> StepResult {
    let ip = match pop16(cpu, mem, cur_addr) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let cs = match pop16(cpu, mem, cur_addr) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if *pop_extra != 0 {
        let sp = (cpu.gpr[RSP] as u16).wrapping_add(*pop_extra);
        cpu.gpr[RSP] = (cpu.gpr[RSP] & !0xFFFF) | sp as u64;
    }
    cpu.cs = cs;
    cpu.rip = ip as u64;
    StepResult::Continue
}
