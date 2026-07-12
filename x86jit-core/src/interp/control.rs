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
