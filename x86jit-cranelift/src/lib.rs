//! Cranelift JIT backend for x86jit (§8.2).
//!
//! Compiles an [`x86jit_core::IrBlock`] to host code. Guest RAM access is inlined
//! (`host_base + guest_addr` after a bounds check); only out-of-range access and
//! syscalls trap out. The compiled-block ABI (signature, result encoding, field
//! offsets) is defined once in `x86jit_core::jit_abi` and shared with the
//! dispatcher; this crate only emits code matching it.
//!
//! Build order (§8.2.3): offsets + ABI + a "returns Continue" block first, then
//! `IrOp`s one at a time, each validated against the interpreter oracle.

#![cfg(feature = "jit")]

mod codegen;
mod perfmap;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use x86jit_core::cache::CompiledPtr;
use x86jit_core::jit_abi::{cpu_offsets, CpuOffsets};
use x86jit_core::{
    Backend, CachedBlock, IrBlock, IrRegion, MemConsistency, RegionCaps, TierUpFinished,
    TierUpRequest, TierUpSubmit, TierUpUnit,
};

/// Division helper called from compiled code (div isn't hot, so a call is fine and
/// avoids 128-bit codegen). Reuses the interpreter's `divide` so both agree.
/// `out` points at `[quot, rem]`; returns 0 on success, 1 on `#DE`.
///
/// # Safety
/// `out` must point at two writable `u64`s. Called only from JIT code with a valid
/// stack-slot pointer.
unsafe extern "C" fn div_helper(
    hi: u64,
    lo: u64,
    divisor: u64,
    size: u64,
    signed: u64,
    out: *mut u64,
) -> u64 {
    match x86jit_core::interp::divide(hi, lo, divisor, size as u8, signed != 0) {
        Some((q, r)) => {
            *out = q;
            *out.add(1) = r;
            0
        }
        None => 1,
    }
}

/// String-op helper: runs the whole (rep) loop via the shared `string_run`. Reads
/// `cpu` and the guest buffer (`MemCtx.base`/`size`); on a trap it writes the
/// fault into `MemCtx` and returns `RET_UNMAPPED`, else `RET_CONTINUE`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn string_helper(
    cpu: *mut u8,
    mem: *mut u8,
    op: u64,
    elem: u64,
    rep: u64,
    cur_addr: u64,
    addr_bits: u64,
    seg_base: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    use x86jit_core::{RepKind, StrOp};

    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let op = [
        StrOp::Movs,
        StrOp::Stos,
        StrOp::Scas,
        StrOp::Cmps,
        StrOp::Lods,
    ][op as usize];
    let rep = [RepKind::None, RepKind::Rep, RepKind::Repe, RepKind::Repne][rep as usize];

    // Raw bounds-only view: the JIT's inlined stores skip SMC/region handling
    // (deferred, §10), so its string helper matches — no `Memory` in the ABI.
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    // task-216: rep movs/stos are the string ops that WRITE guest memory (to RDI); when
    // the embedder is watching a range, record the destination span so JIT'd string
    // stores show up in `take_dirty_ranges` like interpreter ones. RDI (gpr[7]) bounds
    // the written region; snapshot it around the run and mark [min,max)+elem — an
    // over-approximation by at most one element, which is safe (conservative) for dirty
    // tracking. Gated on a LIVE load of `watch_count` through the MemCtx pointer (task-217),
    // so a watch installed by another thread mid-run is seen; an unwatched run does nothing
    // extra beyond the load.
    let track = matches!(op, StrOp::Movs | StrOp::Stos)
        && unsafe { &*(ctx.watch_count_ptr as *const std::sync::atomic::AtomicUsize) }
            .load(std::sync::atomic::Ordering::Relaxed)
            != 0;
    let rdi0 = cpu.gpr[7];
    let ret = match x86jit_core::interp::string_run(
        cpu,
        &raw,
        op,
        elem as u8,
        rep,
        cur_addr,
        addr_bits as u8,
        seg_base,
    ) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    };
    if track {
        let rdi1 = cpu.gpr[7];
        if rdi1 != rdi0 {
            let lo = rdi0.min(rdi1);
            let hi = rdi0.max(rdi1).saturating_add(elem);
            // SAFETY: `mem_self` is the live `&Memory` for this run (set by for_memory).
            let mem = &*(ctx.mem_self as *const x86jit_core::memory::Memory);
            mem.note_watched_write(lo, (hi - lo) as usize);
        }
    }
    ret
}

/// x87 helper: runs one FPU op via the shared `exec_x87`. On a memory fault it
/// writes the fault into `MemCtx`, sets RIP to the faulting instruction, and
/// returns `RET_UNMAPPED`; otherwise `RET_CONTINUE`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call; `kind`
/// is a valid `FpuKind` discriminant (the lift only emits real ones).
unsafe extern "C" fn x87_helper(
    cpu: *mut u8,
    mem: *mut u8,
    kind: u64,
    addr: u64,
    sti: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    // Safe: `kind` came from a real `FpuKind as u16` baked by the lift.
    let kind: x86jit_core::x87::FpuKind = std::mem::transmute(kind as u16);
    let raw = x86jit_core::x87::RawFpMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::x87::exec_x87(cpu, &raw, kind, addr, sti as u8) {
        None => RET_CONTINUE,
        Some((fault, write)) => {
            ctx.fault_addr = fault;
            ctx.fault_access = write as u64;
            cpu.rip = cur_addr;
            RET_UNMAPPED
        }
    }
}

/// fxsave/fxrstor helper: runs the 512-byte save/restore via the shared
/// `exec_fxstate`. On a memory fault it sets RIP + fault fields and returns
/// `RET_UNMAPPED`.
///
/// # Safety
/// `cpu`/`mem` are valid for the call; `mem` is a `*mut MemCtx`.
unsafe extern "C" fn fxstate_helper(
    cpu: *mut u8,
    mem: *mut u8,
    addr: u64,
    restore: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::x87::RawFpMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::x87::exec_fxstate(cpu, &raw, addr, restore != 0) {
        None => RET_CONTINUE,
        Some((fault, write)) => {
            ctx.fault_addr = fault;
            ctx.fault_access = write as u64;
            cpu.rip = cur_addr;
            RET_UNMAPPED
        }
    }
}

/// `cpuid` helper: delegates to the shared `cpuid_run` so both backends report the
/// same features.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn cpuid_helper(cpu: *mut u8) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::cpuid_run(cpu);
}

/// `xgetbv` helper: delegates to the shared `xgetbv_run` so XCR0 tracks the guest
/// feature set (task-169) identically on both backends.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn xgetbv_helper(cpu: *mut u8) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::xgetbv_run(cpu);
}

/// task-216 helper: record a JIT-inlined guest store into the embedder's watched data
/// ranges via `Memory::note_watched_write`. Called from generated store code only when a
/// LIVE load of `Memory::watch_count` (through `MemCtx.watch_count_ptr`, task-217) is
/// non-zero, so it is off the hot path for an unwatched memory.
///
/// # Safety
/// `mem_self` is the live `*const Memory` for this run (set by `MemCtx::for_memory`).
unsafe extern "C" fn note_watched_write_helper(mem_self: u64, addr: u64, len: u64) {
    let mem = &*(mem_self as *const x86jit_core::memory::Memory);
    mem.note_watched_write(addr, len as usize);
}

/// `crc32` helper: CRC-32C folding via the shared `crc32c` so both backends agree.
extern "C" fn crc32_helper(crc: u64, src: u64, bytes: u64) -> u64 {
    x86jit_core::interp::crc32c(crc as u32, src, bytes as u8) as u64
}

/// BMI1/BMI2 helper (task-168.5.3): runs the shared `bmi_result` so the JIT matches
/// the interpreter exactly (the bextr/bzhi variable shift+mask is fiddly to emit
/// natively). Writes `out[0] = result`, `out[1] = CF`; ZF/SF are derived at the call
/// site. `op` is the `BmiOp` discriminant.
///
/// # Safety
/// `out` points to two writable `u64`s for the call.
unsafe extern "C" fn bmi_helper(a: u64, b: u64, op: u64, size: u64, out: *mut u64) {
    use x86jit_core::ir::BmiOp::*;
    let bmiop = match op {
        0 => Andn,
        1 => Blsi,
        2 => Blsr,
        3 => Blsmsk,
        4 => Bextr,
        5 => Bzhi,
        6 => Pdep,
        _ => Pext,
    };
    let (r, cf) = x86jit_core::interp::bmi_result(a, b, size as u8, bmiop);
    *out = r;
    *out.add(1) = cf as u64;
}

/// EVEX masked move helper (task-170.1): runs the shared `CpuState::write_masked`, so
/// the JIT's masking is bit-identical to the interpreter's (masked ops aren't hot, so
/// a helper call beats hand-emitting a per-lane blend). Args are widened to u64.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vmaskmov_helper(
    cpu: *mut u8,
    dst: u64,
    src: u64,
    k: u64,
    elem: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let newval = cpu.vec_lanes(src as usize);
    cpu.write_masked(
        dst as usize,
        newval,
        k as u8,
        elem as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// EVEX write-masked vector **memory** move `vmovdqu{8,16,32,64} v{k}{z}, [mem]` (load)
/// and `[mem]{k}, v` (store) (task-168.5.5). Element-wise via the shared
/// `masked_load_run`/`masked_store_run` so masked-off lanes never fault (hardware
/// suppression) and JIT == interpreter. On an active-lane fault, writes the fault into
/// `MemCtx` (RIP already set by the shared fn) and returns `RET_UNMAPPED`; else
/// `RET_CONTINUE`. Uses the bounds-only `RawStrMem` view, like `string_helper`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vmaskmov_mem_helper(
    cpu: *mut u8,
    mem: *mut u8,
    reg: u64,
    addr: u64,
    k: u64,
    elem: u64,
    zeroing: u64,
    bytes: u64,
    is_store: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    let km = cpu.kmask[k as usize];
    let fault = if is_store != 0 {
        x86jit_core::interp::masked_store_run(
            cpu,
            &raw,
            reg as u8,
            addr,
            km,
            elem as u8,
            bytes as u16,
            cur_addr,
        )
    } else {
        x86jit_core::interp::masked_load_run(
            cpu,
            &raw,
            reg as u8,
            addr,
            km,
            elem as u8,
            zeroing != 0,
            bytes as u16,
            cur_addr,
        )
    };
    match fault {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// Masked EVEX logic (task-168.5.5): compute `op(a, b)` then masked-write into `dst`,
/// via the shared `exec_masked_logic` so JIT == interpreter.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vmasked_logic_helper(
    cpu: *mut u8,
    op_code: u64,
    dst: u64,
    a: u64,
    b: u64,
    k: u64,
    elem: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_masked_logic(
        cpu,
        op_code as u8,
        dst as u8,
        a as u8,
        b as u8,
        k as u8,
        elem as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// Masked EVEX packed arithmetic (task-168.5.5): compute the packed op then masked-write
/// into `dst`, via the shared `exec_masked_packed` so JIT == interpreter.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vmasked_packed_helper(
    cpu: *mut u8,
    op_code: u64,
    dst: u64,
    a: u64,
    b: u64,
    k: u64,
    elem: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_masked_packed(
        cpu,
        op_code as u8,
        dst as u8,
        a as u8,
        b as u8,
        k as u8,
        elem as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// EVEX packed shift-by-imm over any width with optional write-masking (task-215):
/// shifts `a` per `elem`-byte lane and commits into `dst`, via the shared
/// `exec_masked_shift` so JIT == interpreter.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vmasked_shift_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    imm: u64,
    elem: u64,
    right: u64,
    arith: u64,
    k: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_masked_shift(
        cpu,
        dst as u8,
        a as u8,
        imm as u8,
        elem as u8,
        right != 0,
        arith != 0,
        k as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// AVX2/AVX-512 per-element variable shift `vp{sll,srl,sra}v{w,d,q}` (task-215), via the
/// shared `exec_var_shift` → jit == interp. `count` is the count-vector register index.
///
/// # Safety
/// `cpu` is a valid `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn var_shift_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    count: u64,
    elem: u64,
    right: u64,
    arith: u64,
    k: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_var_shift(
        cpu,
        dst as u8,
        a as u8,
        count as u8,
        elem as u8,
        right != 0,
        arith != 0,
        k as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// Packed shift by a scalar register count `vp{sll,srl,sra}{w,d,q} v,v,xmm` (task-215), via
/// the shared `exec_shift_reg` → jit == interp. `count` is the count-xmm register index.
///
/// # Safety
/// `cpu` is a valid `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn shift_reg_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    count: u64,
    elem: u64,
    right: u64,
    arith: u64,
    k: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_shift_reg(
        cpu,
        dst as u8,
        a as u8,
        count as u8,
        elem as u8,
        right != 0,
        arith != 0,
        k as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// GFNI wide/masked `gf2p8{mulb,affineqb,affineinvqb}` (task-215), via the shared
/// `exec_gf2p8` → jit == interp. `mode` is the [`x86jit_core::GfniOp`] wire value.
///
/// # Safety
/// `cpu` is a valid `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn gf2p8_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    b: u64,
    imm: u64,
    mode: u64,
    k: u64,
    zeroing: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_gf2p8(
        cpu,
        dst as u8,
        a as u8,
        b as u8,
        imm as u8,
        mode as u8,
        k as u8,
        zeroing != 0,
        bytes as u16,
    );
}

/// GFNI wide/masked with a memory matrix `vgf2p8affineqb ymm,ymm,[mem]` (task-215), via the
/// shared `gf2p8_mem_run` over the guest buffer. Handles the `dst == src1` aliasing case.
///
/// # Safety
/// `cpu` is a valid `CpuState`; `mem` is a valid `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn gf2p8_mem_helper(
    cpu: *mut u8,
    mem: *mut u8,
    dst: u64,
    a: u64,
    addr: u64,
    imm: u64,
    mode: u64,
    k: u64,
    zeroing: u64,
    bytes: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::interp::gf2p8_mem_run(
        cpu,
        &raw,
        dst as u8,
        a as u8,
        addr,
        imm as u8,
        mode as u8,
        k as u8,
        zeroing != 0,
        bytes as u16,
    ) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// SSE4.2 `pcmpistri`/`pcmpestri` (task-168.5.4): the string-aggregation index + flags,
/// via the shared `pcmpstr_run`. Writes `out[0] = ecx`, `out[1] = cf|zf<<1|sf<<2|of<<3`;
/// the codegen stores ECX and the flags through its own GPR/flag machinery.
///
/// # Safety
/// `cpu` is a valid `CpuState` for the call; `out` points at two writable `u64`s.
unsafe extern "C" fn pcmpstr_helper(
    cpu: *const u8,
    a: u64,
    b: u64,
    imm: u64,
    explicit: u64,
    out: *mut u64,
) {
    let cpu = &*(cpu as *const x86jit_core::state::CpuState);
    let (ecx, cf, zf, sf, of) =
        x86jit_core::interp::pcmpstr_run(cpu, a as u8, b as u8, imm as u8, explicit != 0);
    *out = ecx as u64;
    *out.add(1) = (cf as u64) | ((zf as u64) << 1) | ((sf as u64) << 2) | ((of as u64) << 3);
}

/// Memory-source `pcmpistri`/`pcmpestri` (task-195): source 2 is supplied as the loaded
/// 128-bit value (`bv_lo`/`bv_hi`) rather than a register; source 1 is `cpu.xmm[a]`. The
/// JIT loads (and fault-checks) the operand before the call. Out-slot layout matches
/// [`pcmpstr_helper`].
///
/// # Safety
/// `cpu` is a valid `CpuState` for the call; `out` points at two writable `u64`s.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn pcmpstr_mem_helper(
    cpu: *const u8,
    a: u64,
    bv_lo: u64,
    bv_hi: u64,
    imm: u64,
    explicit: u64,
    out: *mut u64,
) {
    let cpu = &*(cpu as *const x86jit_core::state::CpuState);
    let bv = (bv_lo as u128) | ((bv_hi as u128) << 64);
    let (ecx, cf, zf, sf, of) =
        x86jit_core::interp::pcmpstr_run_bv(cpu, a as u8, bv, imm as u8, explicit != 0);
    *out = ecx as u64;
    *out.add(1) = (cf as u64) | ((zf as u64) << 1) | ((sf as u64) << 2) | ((of as u64) << 3);
}

/// SSE4.2 `pcmpistrm`/`pcmpestrm` (task-195): the string-aggregation MASK (written to XMM0)
/// plus flags, via the shared `pcmpstrm_run`. The helper writes XMM0 directly (`&mut cpu`)
/// and returns the flags in `out[1] = cf|zf<<1|sf<<2|of<<3`; the codegen stores the flags.
///
/// # Safety
/// `cpu` is a valid `CpuState` for the call; `out` points at two writable `u64`s.
unsafe extern "C" fn pcmpstrm_helper(
    cpu: *mut u8,
    a: u64,
    b: u64,
    imm: u64,
    explicit: u64,
    out: *mut u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let (mask, cf, zf, sf, of) =
        x86jit_core::interp::pcmpstrm_run(cpu, a as u8, b as u8, imm as u8, explicit != 0);
    cpu.xmm[0] = mask;
    *out.add(1) = (cf as u64) | ((zf as u64) << 1) | ((sf as u64) << 2) | ((of as u64) << 3);
}

/// Memory-source `pcmpistrm`/`pcmpestrm` (task-195): source 2 is the loaded 128-bit value.
/// Out-slot + XMM0 layout matches [`pcmpstrm_helper`].
///
/// # Safety
/// `cpu` is a valid `CpuState` for the call; `out` points at two writable `u64`s.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn pcmpstrm_mem_helper(
    cpu: *mut u8,
    a: u64,
    bv_lo: u64,
    bv_hi: u64,
    imm: u64,
    explicit: u64,
    out: *mut u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let bv = (bv_lo as u128) | ((bv_hi as u128) << 64);
    let (mask, cf, zf, sf, of) =
        x86jit_core::interp::pcmpstrm_run_bv(cpu, a as u8, bv, imm as u8, explicit != 0);
    cpu.xmm[0] = mask;
    *out.add(1) = (cf as u64) | ((zf as u64) << 1) | ((sf as u64) << 2) | ((of as u64) << 3);
}

/// EVEX `valign{d,q}` (task-168.5.6): cross-lane element shift, via the shared
/// `exec_valign` so JIT == interpreter.
unsafe extern "C" fn valign_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    b: u64,
    shift: u64,
    elem: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_valign(
        cpu,
        dst as u8,
        a as u8,
        b as u8,
        shift as u8,
        elem as u8,
        bytes as u16,
    );
}

/// `vpermt2{b,w,d,q}` helper (task-195): two-table cross-lane permute via the shared
/// `exec_vpermt2` so JIT == interpreter. Writes the dst vector reg in CpuState directly
/// (vector state is memory-backed); GPRs untouched.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vpermt2_helper(
    cpu: *mut u8,
    dst: u64,
    idx: u64,
    tbl: u64,
    elem: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
    bytes: u64,
    imode: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vpermt2(
        cpu,
        dst as u8,
        idx as u8,
        tbl as u8,
        elem as u8,
        k as u8,
        masked != 0,
        zeroing != 0,
        bytes as u16,
        imode != 0,
    );
}

/// Memory-source `vpermt2`/`vpermi2` helper (task-195): table 1 is loaded from `[addr]`
/// via the shared `permute2_run` (jit == interp). Fault-capable: returns `RET_UNMAPPED`
/// with the fault recorded in the `MemCtx`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vpermt2_mem_helper(
    cpu: *mut u8,
    mem: *mut u8,
    dst: u64,
    idx: u64,
    addr: u64,
    elem: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
    bytes: u64,
    imode: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::interp::permute2_run(
        cpu,
        &raw,
        dst as u8,
        idx as u8,
        addr,
        elem as u8,
        k as u8,
        masked != 0,
        zeroing != 0,
        bytes as u16,
        imode != 0,
        cur_addr,
    ) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// Single-source cross-lane permute `vperm{d,q}` helper (task-195): via the shared
/// `exec_vperm1` so JIT == interpreter. Writes the dst vector reg (memory-backed).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vperm1_helper(
    cpu: *mut u8,
    dst: u64,
    idx: u64,
    src: u64,
    elem: u64,
    bytes: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vperm1(
        cpu,
        dst as u8,
        idx as u8,
        src as u8,
        elem as u8,
        bytes as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// Memory-source single-table permute `vperm{d,q} v, idx, [mem]` helper (task-215): the
/// table is loaded from `[addr]` via the shared `vperm1_run` (jit == interp).
/// Fault-capable: returns `RET_UNMAPPED` with the fault recorded in the `MemCtx`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vperm1_mem_helper(
    cpu: *mut u8,
    mem: *mut u8,
    dst: u64,
    idx: u64,
    addr: u64,
    elem: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
    bytes: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::interp::vperm1_run(
        cpu,
        &raw,
        dst as u8,
        idx as u8,
        addr,
        elem as u8,
        k as u8,
        masked != 0,
        zeroing != 0,
        bytes as u16,
        cur_addr,
    ) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// `vpmov{q,d,w}{d,w,b}` narrowing-move helper (task-195): truncate + pack via the
/// shared `exec_vpmov_narrow` so JIT == interpreter. Writes the dst vector reg in
/// CpuState directly (vector state is memory-backed); GPRs untouched.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vpmov_narrow_helper(
    cpu: *mut u8,
    dst: u64,
    src: u64,
    from: u64,
    to: u64,
    src_width: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vpmov_narrow(
        cpu,
        dst as u8,
        src as u8,
        from as u8,
        to as u8,
        src_width as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// Narrowing-store helper `vpmov{q,d,w}{d,w,b} [mem], src` (task-195, unmasked): truncate
/// then store contiguously via the shared `narrow_store_run` (jit == interp). Fault-capable:
/// returns `RET_UNMAPPED` with the fault address recorded in the `MemCtx`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vpmov_narrow_mem_helper(
    cpu: *mut u8,
    mem: *mut u8,
    src: u64,
    addr: u64,
    from: u64,
    to: u64,
    src_width: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::interp::narrow_store_run(
        cpu,
        &raw,
        src as u8,
        from as u8,
        to as u8,
        src_width as u16,
        addr,
        cur_addr,
    ) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// `vpmov{s,z}x*` widening-move helper for wide/masked dests (task-195): zero/sign-extend
/// via the shared `exec_vpmov_extend_wide` so JIT == interpreter. Writes the dst vector
/// reg in CpuState directly (vector state is memory-backed); GPRs untouched.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vpmov_extend_wide_helper(
    cpu: *mut u8,
    dst: u64,
    src: u64,
    from: u64,
    to: u64,
    signed: u64,
    dst_width: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vpmov_extend_wide(
        cpu,
        dst as u8,
        src as u8,
        from as u8,
        to as u8,
        signed != 0,
        dst_width as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// `vpabs{b,w,d,q}` packed absolute-value helper (task-195): via the shared `exec_vpabs`
/// so JIT == interpreter. Writes the dst vector reg in CpuState directly (memory-backed).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vpabs_helper(
    cpu: *mut u8,
    dst: u64,
    src: u64,
    elem: u64,
    dst_width: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vpabs(
        cpu,
        dst as u8,
        src as u8,
        elem as u8,
        dst_width as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// Masked EVEX unary lane helper `vplzcnt/vprol/vpconflict` (task-209): via the shared
/// `exec_vp_unary_lane` so JIT == interpreter. `op` is the [`x86jit_core::ir::VpUnaryOp`]
/// wire value; `imm` is the rotate count (vprol only). Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vp_unary_lane_helper(
    cpu: *mut u8,
    dst: u64,
    src: u64,
    op: u64,
    imm: u64,
    elem: u64,
    dst_width: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vp_unary_lane(
        cpu,
        dst as u8,
        src as u8,
        x86jit_core::ir::VpUnaryOp::from_u8(op as u8),
        imm as u8,
        elem as u8,
        dst_width as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// Masked EVEX blend helper `vpblendm{d,q}` (task-209): via the shared `exec_vp_blendm`
/// so JIT == interpreter. `k` is the blend-control opmask. Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vp_blendm_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    b: u64,
    k: u64,
    elem: u64,
    dst_width: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vp_blendm(
        cpu,
        dst as u8,
        a as u8,
        b as u8,
        k as u8,
        elem as u8,
        dst_width as u16,
        zeroing != 0,
    );
}

/// Masked EVEX 128-bit-lane shuffle helper `vshuff32x4/64x2` (task-209): via the shared
/// `exec_vshuf_lane` so JIT == interpreter. Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vshuf_lane_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    b: u64,
    imm: u64,
    elem: u64,
    dst_width: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vshuf_lane(
        cpu,
        dst as u8,
        a as u8,
        b as u8,
        imm as u8,
        elem as u8,
        dst_width as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// Masked EVEX `vpmultishiftqb` helper (VBMI, task-209): via the shared
/// `exec_vp_multishift` so JIT == interpreter. Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vp_multishift_helper(
    cpu: *mut u8,
    dst: u64,
    ctrl: u64,
    data: u64,
    dst_width: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vp_multishift(
        cpu,
        dst as u8,
        ctrl as u8,
        data as u8,
        dst_width as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// `vpshufb` (EVEX) per-lane byte-shuffle helper (task-195): via the shared
/// `exec_vpshufb_wide` so JIT == interpreter. Writes the dst vector reg (memory-backed).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vpshufb_wide_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    idx: u64,
    bytes: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vpshufb_wide(
        cpu,
        dst as u8,
        a as u8,
        idx as u8,
        bytes as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// `vpshufd` (EVEX/VEX-256) per-lane dword-shuffle helper (task-195): via the shared
/// `exec_vshuffle32_wide` so JIT == interpreter. Writes the dst vector reg (memory-backed).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn vshuffle32_wide_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    imm: u64,
    bytes: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vshuffle32_wide(
        cpu,
        dst as u8,
        a as u8,
        imm as u8,
        bytes as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// `pack{ss,us}{wb,dw}` saturating-pack helper (task-195): via the shared `exec_vpack`
/// so JIT == interpreter. Writes the dst vector reg (memory-backed).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn vpack_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    b: u64,
    from_elem: u64,
    signed: u64,
    bytes: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_vpack(
        cpu,
        dst as u8,
        a as u8,
        b as u8,
        from_elem as u8,
        signed != 0,
        bytes as u16,
    );
}

/// Memory-source variant of [`vpack_helper`] (task-243): the 128-bit second source is
/// passed as two i64 halves (loaded — and fault-checked — in JIT code). `dst` already
/// holds source 1 (pre-copied by the lift), so this packs `dst = pack(dst, b)`.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn vpack_mem_helper(
    cpu: *mut u8,
    dst: u64,
    lo: u64,
    hi: u64,
    from_elem: u64,
    signed: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let b = (lo as u128) | ((hi as u128) << 64);
    x86jit_core::interp::pack_wide_mem(cpu, dst as u8, b, from_elem as u8, signed != 0);
}

/// SSE3 lane-combining packed float helper (register form, task-244): `haddp`/`hsubp`/
/// `addsubp` via the shared `hfloat_reg` so JIT == interpreter.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn vhfloat_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    b: u64,
    op: u64,
    f64_prec: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let prec = if f64_prec != 0 {
        x86jit_core::FPrec::F64
    } else {
        x86jit_core::FPrec::F32
    };
    let op = x86jit_core::interp::hfloat_op_from_code(op as u8);
    x86jit_core::interp::hfloat_reg(cpu, dst as u8, a as u8, b as u8, op, prec);
}

/// Memory-source variant of [`vhfloat_helper`] (task-244): the 128-bit second source is
/// passed as two i64 halves (loaded — and fault-checked — in JIT code). `dst` already
/// holds op1 (pre-copied by the lift).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn vhfloat_mem_helper(
    cpu: *mut u8,
    dst: u64,
    lo: u64,
    hi: u64,
    op: u64,
    f64_prec: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let b = (lo as u128) | ((hi as u128) << 64);
    let prec = if f64_prec != 0 {
        x86jit_core::FPrec::F64
    } else {
        x86jit_core::FPrec::F32
    };
    let op = x86jit_core::interp::hfloat_op_from_code(op as u8);
    x86jit_core::interp::hfloat_mem(cpu, dst as u8, b, op, prec);
}

/// SSSE3 packed-integer horizontal helper (register form, task-247): `phaddw/d/sw`,
/// `phsubw/d/sw` via the shared `hint_reg` so JIT == interpreter.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn vhint_helper(cpu: *mut u8, dst: u64, a: u64, b: u64, op: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let op = x86jit_core::interp::hint_op_from_code(op as u8);
    x86jit_core::interp::hint_reg(cpu, dst as u8, a as u8, b as u8, op);
}

/// Memory-source variant of [`vhint_helper`] (task-247): the 128-bit second source is
/// passed as two i64 halves (loaded — and fault-checked — in JIT code). `dst` already
/// holds op1 (pre-copied by the lift).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn vhint_mem_helper(cpu: *mut u8, dst: u64, lo: u64, hi: u64, op: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let b = (lo as u128) | ((hi as u128) << 64);
    let op = x86jit_core::interp::hint_op_from_code(op as u8);
    x86jit_core::interp::hint_mem(cpu, dst as u8, b, op);
}

/// `pmaddwd` multiply-add helper (task-190): via the shared `exec_pmaddwd` so
/// JIT == interpreter. Writes the dst vector reg (memory-backed).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn pmaddwd_helper(cpu: *mut u8, dst: u64, a: u64, b: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_pmaddwd(cpu, dst as u8, a as u8, b as u8);
}

/// EVEX lane-broadcast helper (register form, task-214): via the shared
/// `exec_broadcast_lane` so JIT == interpreter. Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn broadcast_lane_helper(
    cpu: *mut u8,
    dst: u64,
    src: u64,
    chunk: u64,
    elem: u64,
    dst_width: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_broadcast_lane(
        cpu,
        dst as u8,
        src as u8,
        chunk as u8,
        elem as u8,
        dst_width as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// EVEX lane-broadcast helper (memory form, task-214): loads the chunk from `[base]` via
/// the shared `broadcast_lane_mem_run` (jit == interp). Fault-capable: returns
/// `RET_UNMAPPED` with the fault recorded in the `MemCtx`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn broadcast_lane_mem_helper(
    cpu: *mut u8,
    mem: *mut u8,
    dst: u64,
    base: u64,
    chunk: u64,
    elem: u64,
    dst_width: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    match x86jit_core::interp::broadcast_lane_mem_run(
        cpu,
        &raw,
        dst as u8,
        base,
        chunk as u8,
        elem as u8,
        dst_width as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
        cur_addr,
    ) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// FMA3 helper (register form, task-201): fused multiply-add via the shared `exec_fma`
/// so JIT == interpreter. Writes the dst vector reg (memory-backed).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn fma_helper(
    cpu: *mut u8,
    dst: u64,
    x: u64,
    y: u64,
    z: u64,
    prec_f64: u64,
    scalar: u64,
    neg_prod: u64,
    neg_add: u64,
    bytes: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_fma(
        cpu,
        dst as u8,
        x as u8,
        y as u8,
        z as u8,
        prec_f64 != 0,
        scalar != 0,
        neg_prod != 0,
        neg_add != 0,
        bytes as u16,
        k as u8,
        masked != 0,
        zeroing != 0,
    );
}

/// FMA3 memory-form helper (task-201): one source is loaded from `[base]` via the shared
/// `fma_mem_run` (jit == interp). Fault-capable: returns `RET_UNMAPPED` with the fault
/// recorded in the `MemCtx`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn fma_mem_helper(
    cpu: *mut u8,
    mem: *mut u8,
    dst: u64,
    x: u64,
    y: u64,
    z: u64,
    base: u64,
    mem_role: u64,
    prec_f64: u64,
    scalar: u64,
    neg_prod: u64,
    neg_add: u64,
    bytes: u64,
    cur_addr: u64,
    k: u64,
    masked: u64,
    zeroing: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
        guest_base: ctx.guest_base,
    };
    let writemask = if masked != 0 { Some(k as u8) } else { None };
    match x86jit_core::interp::fma_mem_run(
        cpu,
        &raw,
        dst as u8,
        x as u8,
        y as u8,
        z as u8,
        base,
        mem_role as u8,
        prec_f64 != 0,
        scalar != 0,
        neg_prod != 0,
        neg_add != 0,
        bytes as u16,
        cur_addr,
        writemask,
        zeroing != 0,
    ) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// AES-NI helper (register form, task-205): dispatches all 6 AES ops via the shared
/// `x86jit_core::aes` primitives so JIT == interpreter. `op`: 0=enc,1=dec,2=enclast,
/// 3=declast,4=imc,5=keygen. Round ops use `a` (state) + `b` (round key); imc/keygen
/// use `a` as the single source (`imm` = RCON for keygen). Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn aes_helper(cpu: *mut u8, dst: u64, a: u64, b: u64, op: u64, imm: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    match op {
        4 => x86jit_core::interp::exec_aes_imc(cpu, dst as u8, a as u8),
        5 => x86jit_core::interp::exec_aes_keygen(cpu, dst as u8, a as u8, imm as u8),
        _ => x86jit_core::interp::exec_aes(cpu, dst as u8, a as u8, b as u8, op as u8),
    }
}

/// AES-NI helper (memory form, task-205): the 128-bit memory source is already loaded
/// (fault handled natively before the call) and passed as `lo`/`hi`. Same op dispatch
/// as [`aes_helper`]; round ops use `a` (state) + the loaded key, imc/keygen use the
/// loaded value as the single source.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn aes_mem_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    lo: u64,
    hi: u64,
    op: u64,
    imm: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let v = (lo as u128) | ((hi as u128) << 64);
    match op {
        4 => x86jit_core::interp::exec_aes_imc_mem(cpu, dst as u8, v),
        5 => x86jit_core::interp::exec_aes_keygen_mem(cpu, dst as u8, v, imm as u8),
        _ => x86jit_core::interp::exec_aes_mem(cpu, dst as u8, a as u8, v, op as u8),
    }
}

/// SHA-NI helper (register form, task-207): dispatches all 7 SHA ops via the shared
/// `x86jit_core::sha` primitives so JIT == interpreter. `op` is the [`ShaOp`] wire value;
/// `a` = op1 (dst), `b` = op2, `imm` = `sha1rnds4`'s immediate. `sha256rnds2` reads xmm0
/// implicitly inside the entry point. Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn sha_helper(cpu: *mut u8, dst: u64, a: u64, b: u64, op: u64, imm: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_sha(cpu, dst as u8, a as u8, b as u8, imm as u8, op as u8);
}

/// SHA-NI helper (memory form, task-207): the 128-bit op2 memory source is already loaded
/// (fault handled natively before the call) and passed as `lo`/`hi`. Same op dispatch as
/// [`sha_helper`]; `sha256rnds2` reads xmm0 implicitly inside the entry point.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn sha_mem_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    lo: u64,
    hi: u64,
    op: u64,
    imm: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let v = (lo as u128) | ((hi as u128) << 64);
    x86jit_core::interp::exec_sha_mem(cpu, dst as u8, a as u8, v, imm as u8, op as u8);
}

/// GFNI helper (register form, task-210): dispatches `gf2p8mulb/affineqb/affineinvqb`
/// via the shared `x86jit_core::gfni` primitives so JIT == interpreter. `op` is the
/// [`x86jit_core::GfniOp`] wire value; `a` = op1, `b` = op2, `imm` = affine constant.
/// Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn gfni_helper(cpu: *mut u8, dst: u64, a: u64, b: u64, op: u64, imm: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_gfni(cpu, dst as u8, a as u8, b as u8, imm as u8, op as u8);
}

/// GFNI helper (memory form, task-210): the 128-bit op2 memory source is already loaded
/// (fault handled natively before the call) and passed as `lo`/`hi`. Same op dispatch
/// as [`gfni_helper`].
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn gfni_mem_helper(
    cpu: *mut u8,
    dst: u64,
    a: u64,
    lo: u64,
    hi: u64,
    op: u64,
    imm: u64,
) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let v = (lo as u128) | ((hi as u128) << 64);
    x86jit_core::interp::exec_gfni_mem(cpu, dst as u8, a as u8, v, imm as u8, op as u8);
}

/// PCLMULQDQ helper (register form, task-211): carry-less multiply via the shared
/// `x86jit_core::pclmul` primitive so JIT == interpreter. `a` = op1, `b` = op2, `imm`
/// selects the 64-bit halves. Register-only, never faults.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn pclmul_helper(cpu: *mut u8, dst: u64, a: u64, b: u64, imm: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::exec_pclmul(cpu, dst as u8, a as u8, b as u8, imm as u8);
}

/// PCLMULQDQ helper (memory form, task-211): the 128-bit op2 memory source is already
/// loaded (fault handled natively before the call) and passed as `lo`/`hi`. Same as
/// [`pclmul_helper`] with the loaded value as op2.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn pclmul_mem_helper(cpu: *mut u8, dst: u64, a: u64, lo: u64, hi: u64, imm: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let v = (lo as u128) | ((hi as u128) << 64);
    x86jit_core::interp::exec_pclmul_mem(cpu, dst as u8, a as u8, v, imm as u8);
}

/// MMX↔XMM bridge helper (task-208): `op` 0 = `movq2dq` (a=dst_xmm, b=src_mm), 1 =
/// `movdq2q` (a=dst_mm, b=src_xmm). Touches `cpu.xmm`/`cpu.fpr` (memory-backed).
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn mmx_bridge_helper(cpu: *mut u8, op: u64, a: u64, b: u64) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    if op == 0 {
        x86jit_core::interp::exec_movq2dq(cpu, a as u8, b as u8);
    } else {
        x86jit_core::interp::exec_movdq2q(cpu, a as u8, b as u8);
    }
}

/// Bounded background-compile queue depth (bg-tier, doc-27 D4): a full queue makes
/// `tier_up_async` return `Busy` and the block stays interpreted — never an inline
/// compile spike under peak pressure.
const TIER_QUEUE_CAP: usize = 64;

/// Which *host* instruction set Cranelift may emit for the guest IR (task-175) — the
/// host-codegen axis, orthogonal to the guest ISA (`GuestCpuFeatures`, task-169).
/// Lives on [`JitBackend`], not `VmConfig`: the interpreter has no codegen target (it
/// is plain Rust fixed at compile time), so this would be meaningless on the shared
/// config. Guest-invisible — only instruction selection changes, not results — so the
/// interpreter stays the reference oracle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostTarget {
    /// Detect the running host and use all its features (`cranelift_native`) — the
    /// default, and the pre-task-175 behavior. A hot loop tiers into host-optimal code.
    #[default]
    Native,
    /// Forbid AVX and above (AVX2/AVX-512/FMA): deterministic, AOT-cacheable output
    /// that runs on any SSE4.1 host. Guest AVX/AVX-512 still executes — Cranelift
    /// lowers our 128-bit-lane IR to SSE.
    Baseline,
}

/// The JIT backend. Injected into a `Vm` via `Vm::with_backend` (§4.1) — the core
/// never names this type. Owns the executable-memory arena (`JITModule`) and
/// Cranelift context behind a `Mutex`, so `materialize(&self)` stays `Send + Sync`
/// for a shared `Vm`. With background tier-up (doc-27 D3) it also owns a compiler
/// worker thread and the queues feeding it.
pub struct JitBackend {
    shared: Arc<Shared>,
    /// The background compiler thread (bg-tier, doc-27 D3), spawned lazily on the
    /// first [`tier_up_async`](Backend::tier_up_async) and joined on `Drop`. `None`
    /// until a background tier-up is first requested — eager/sync use never spawns.
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// State shared between the vcpu threads (foreground `materialize`, submit, drain)
/// and the background compiler worker (bg-tier, doc-27 D3). Behind `Arc` so a
/// [`TierUpHandle`] clone can expose `wait_idle` without owning the worker thread.
struct Shared {
    inner: Mutex<Jit>,
    offsets: CpuOffsets,
    /// Superblock caps (§12 M5-T3), or `None` to compile one block at a time.
    caps: Option<RegionCaps>,
    /// Submitted-but-not-completed requests + worker coordination. The worker sleeps
    /// on `work_cv`; `wait_idle` sleeps on `idle_cv`.
    queue: Mutex<Queue>,
    work_cv: Condvar,
    idle_cv: Condvar,
    /// Finished compiles awaiting the core dispatcher's drain (decision-5).
    done: Mutex<Vec<TierUpFinished>>,
    /// Lock-free "anything to drain?" probe, kept equal to `done.len()` under the
    /// `done` lock — lets `tier_up_finished` early-out without locking.
    ready: AtomicUsize,
    /// Total nanoseconds spent in `compile_with` (every foreground / tier-up / bg
    /// compile), for the bench's compile-vs-run split (perf-bench v2, PB-2). Relaxed:
    /// a monotone accumulator, not a synchronization channel.
    compile_ns: AtomicU64,
}

/// The background compile queue and its liveness counters (bg-tier, doc-27 D3/D4).
struct Queue {
    items: VecDeque<TierUpRequest>,
    /// Requests submitted but not yet completed (queued + the one compiling).
    /// `wait_idle` blocks until this reaches zero.
    outstanding: usize,
    /// Set by `Drop` to unblock and stop the worker.
    shutdown: bool,
    /// Test lever (bg-tier BGT-4): while true the worker parks without popping, so
    /// requests pile up in the queue. Toggled via `TierUpHandle::pause_compiler`;
    /// `shutdown` still wins so `Drop` never hangs. Never set in production.
    paused: bool,
}

struct Jit {
    module: JITModule,
    fbctx: FunctionBuilderContext,
    next_id: u64,
    // Link slots for block chaining (§12 M5). Each `Box<AtomicU64>` holds a
    // compiled entry pointer (0 = unlinked); its heap address is baked into the
    // code and filled by the dispatcher. Owned here so it lives as long as the Vm.
    // The `Box` is load-bearing: a bare `Vec` would move its elements on growth,
    // invalidating the addresses already baked into compiled code.
    //
    // `AtomicU64` (not plain `u64`): the dispatcher fill and the SMC-driven clear
    // (`invalidate_links`, R1) both store atomically, so a vcpu reading the slot
    // from compiled code sees 0 or a valid entry, never a torn value. Compiled-code
    // loads are plain machine loads (aligned u64 is naturally atomic on the hosts
    // we target); only the Rust-side writes need the atomic type.
    #[allow(clippy::vec_box)]
    slots: Vec<Box<AtomicU64>>,
}

impl JitBackend {
    pub fn new() -> Self {
        Self::build(None, HostTarget::Native)
    }

    /// A JIT that forms superblocks (§12 M5-T3): the dispatcher lifts a region and
    /// compiles it as one function, up to `caps`. Opt-in until M5-T3f flips the
    /// default on.
    pub fn with_superblocks(caps: RegionCaps) -> Self {
        Self::build(Some(caps), HostTarget::Native)
    }

    /// A JIT pinned to a [`HostTarget`] (task-175): which *host* instructions Cranelift
    /// may emit for the guest IR — a separate axis from the guest ISA
    /// (`GuestCpuFeatures`, task-169). Default is [`HostTarget::Native`] (detect the
    /// running host). Guest-invisible: the emitted code is bit-identical in effect
    /// (only instruction *selection* changes), so interp == JIT holds regardless.
    pub fn with_host_target(target: HostTarget) -> Self {
        Self::build(None, target)
    }

    fn build(caps: Option<RegionCaps>, target: HostTarget) -> Self {
        let mut flags = settings::builder();
        flags.set("use_colocated_libcalls", "false").unwrap();
        flags.set("is_pic", "false").unwrap();
        let mut isa_builder = cranelift_native::builder().expect("host ISA");
        if target == HostTarget::Baseline {
            // Pin below the host: forbid AVX and above so codegen is deterministic and
            // portable to any SSE4.1 host (an AOT cache built here runs on older CPUs).
            // Cranelift lowers our 128-bit-lane vector IR to SSE, so guest AVX/AVX-512
            // still executes correctly — just via SSE host instructions. FMA off too:
            // no mul+add contraction, so results stay bit-identical to the interpreter.
            for flag in [
                "has_avx",
                "has_avx512bitalg",
                "has_avx512dq",
                "has_avx512f",
                "has_avx512vbmi",
                "has_avx512vl",
                "has_fma",
            ] {
                let _ = isa_builder.set(flag, "false");
            }
        }
        let isa = isa_builder
            .finish(settings::Flags::new(flags))
            .expect("finish ISA");
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol("x86jit_div", div_helper as *const u8);
        builder.symbol("x86jit_string", string_helper as *const u8);
        builder.symbol("x86jit_cpuid", cpuid_helper as *const u8);
        builder.symbol("x86jit_xgetbv", xgetbv_helper as *const u8);
        builder.symbol("x86jit_vmaskmov", vmaskmov_helper as *const u8);
        builder.symbol("x86jit_vmasked_logic", vmasked_logic_helper as *const u8);
        builder.symbol("x86jit_vmasked_packed", vmasked_packed_helper as *const u8);
        builder.symbol("x86jit_vmasked_shift", vmasked_shift_helper as *const u8);
        builder.symbol("x86jit_var_shift", var_shift_helper as *const u8);
        builder.symbol("x86jit_shift_reg", shift_reg_helper as *const u8);
        builder.symbol("x86jit_gf2p8", gf2p8_helper as *const u8);
        builder.symbol("x86jit_gf2p8_mem", gf2p8_mem_helper as *const u8);
        builder.symbol("x86jit_valign", valign_helper as *const u8);
        builder.symbol("x86jit_vpermt2", vpermt2_helper as *const u8);
        builder.symbol("x86jit_vpermt2_mem", vpermt2_mem_helper as *const u8);
        builder.symbol("x86jit_vperm1", vperm1_helper as *const u8);
        builder.symbol("x86jit_vperm1_mem", vperm1_mem_helper as *const u8);
        builder.symbol("x86jit_vpmov_narrow", vpmov_narrow_helper as *const u8);
        builder.symbol(
            "x86jit_vpmov_narrow_mem",
            vpmov_narrow_mem_helper as *const u8,
        );
        builder.symbol(
            "x86jit_vpmov_extend_wide",
            vpmov_extend_wide_helper as *const u8,
        );
        builder.symbol("x86jit_vpabs", vpabs_helper as *const u8);
        builder.symbol("x86jit_vp_unary_lane", vp_unary_lane_helper as *const u8);
        builder.symbol("x86jit_vp_blendm", vp_blendm_helper as *const u8);
        builder.symbol("x86jit_vshuf_lane", vshuf_lane_helper as *const u8);
        builder.symbol("x86jit_vp_multishift", vp_multishift_helper as *const u8);
        builder.symbol("x86jit_vpshufb_wide", vpshufb_wide_helper as *const u8);
        builder.symbol(
            "x86jit_vshuffle32_wide",
            vshuffle32_wide_helper as *const u8,
        );
        builder.symbol("x86jit_vpack", vpack_helper as *const u8);
        builder.symbol("x86jit_vpack_mem", vpack_mem_helper as *const u8);
        builder.symbol("x86jit_vhfloat", vhfloat_helper as *const u8);
        builder.symbol("x86jit_vhfloat_mem", vhfloat_mem_helper as *const u8);
        builder.symbol("x86jit_vhint", vhint_helper as *const u8);
        builder.symbol("x86jit_vhint_mem", vhint_mem_helper as *const u8);
        builder.symbol("x86jit_pmaddwd", pmaddwd_helper as *const u8);
        builder.symbol("x86jit_fma", fma_helper as *const u8);

        builder.symbol("x86jit_broadcast_lane", broadcast_lane_helper as *const u8);

        builder.symbol(
            "x86jit_broadcast_lane_mem",
            broadcast_lane_mem_helper as *const u8,
        );
        builder.symbol("x86jit_fma_mem", fma_mem_helper as *const u8);
        builder.symbol("x86jit_aes", aes_helper as *const u8);
        builder.symbol("x86jit_aes_mem", aes_mem_helper as *const u8);
        builder.symbol("x86jit_sha", sha_helper as *const u8);
        builder.symbol("x86jit_sha_mem", sha_mem_helper as *const u8);
        builder.symbol("x86jit_gfni", gfni_helper as *const u8);
        builder.symbol("x86jit_gfni_mem", gfni_mem_helper as *const u8);
        builder.symbol("x86jit_pclmul", pclmul_helper as *const u8);
        builder.symbol("x86jit_pclmul_mem", pclmul_mem_helper as *const u8);
        builder.symbol("x86jit_mmx_bridge", mmx_bridge_helper as *const u8);
        builder.symbol("x86jit_pcmpstr", pcmpstr_helper as *const u8);
        builder.symbol("x86jit_pcmpstr_mem", pcmpstr_mem_helper as *const u8);
        builder.symbol("x86jit_pcmpstrm", pcmpstrm_helper as *const u8);
        builder.symbol("x86jit_pcmpstrm_mem", pcmpstrm_mem_helper as *const u8);
        builder.symbol("x86jit_bmi", bmi_helper as *const u8);
        builder.symbol("x86jit_x87", x87_helper as *const u8);
        builder.symbol("x86jit_fxstate", fxstate_helper as *const u8);
        builder.symbol("x86jit_crc32", crc32_helper as *const u8);
        builder.symbol("x86jit_note_watch", note_watched_write_helper as *const u8);
        let module = JITModule::new(builder);

        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Jit {
                    module,
                    fbctx: FunctionBuilderContext::new(),
                    next_id: 0,
                    slots: Vec::new(),
                }),
                offsets: cpu_offsets(),
                caps,
                queue: Mutex::new(Queue {
                    items: VecDeque::new(),
                    outstanding: 0,
                    shutdown: false,
                    paused: false,
                }),
                work_cv: Condvar::new(),
                idle_cv: Condvar::new(),
                done: Mutex::new(Vec::new()),
                ready: AtomicUsize::new(0),
                compile_ns: AtomicU64::new(0),
            }),
            worker: Mutex::new(None),
        }
    }

    /// Spawn the background compiler thread if it isn't running yet (bg-tier, doc-27
    /// D3). Lazy: eager/sync-only use never reaches here, so it never spawns.
    fn ensure_worker(&self) {
        let mut w = self.worker.lock().unwrap();
        if w.is_none() {
            let shared = Arc::clone(&self.shared);
            *w = Some(
                std::thread::Builder::new()
                    .name("x86jit-tier".into())
                    .spawn(move || shared.worker_loop())
                    .expect("spawn tier-up worker"),
            );
        }
    }

    /// A handle to the background tier-up machinery (bg-tier, doc-27 D6). Its
    /// [`wait_idle`](TierUpHandle::wait_idle) blocks until every submitted compile
    /// has completed — the determinism lever for tests. Grab it before boxing the
    /// backend into a `Vm`.
    pub fn tier_up_handle(&self) -> TierUpHandle {
        TierUpHandle {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Shared {
    fn compile(
        &self,
        ir: &IrBlock,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CompiledPtr {
        self.compile_with(
            perfmap::Kind::Block,
            ir.guest_start,
            |builder, helpers, alloc_slot| {
                codegen::translate_block(
                    builder,
                    ir,
                    &self.offsets,
                    alloc_slot,
                    helpers,
                    consistency,
                    mmio,
                    guest_base,
                );
            },
        )
    }

    /// Compile a superblock region (§12 M5-T3) as one function.
    fn compile_region(
        &self,
        region: &IrRegion,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CompiledPtr {
        self.compile_with(
            perfmap::Kind::Region,
            region.entry,
            |builder, helpers, alloc_slot| {
                codegen::translate_region(
                    builder,
                    region,
                    &self.offsets,
                    alloc_slot,
                    helpers,
                    consistency,
                    mmio,
                    guest_base,
                );
            },
        )
    }

    /// Shared function-building spine: sets up the signature, imports the five
    /// helpers, runs `translate` to emit the body, and finalizes. `translate`
    /// receives the builder, the imported helper refs, and the link-slot allocator.
    fn compile_with(
        &self,
        // task-196: perf-map symbol kind + guest entry PC, threaded through so the
        // emitted symbol names the guest RIP this host code was compiled from. The
        // guest PC is not otherwise in scope here (only the host entry is), so it's
        // passed by the block/region callers.
        perf_kind: perfmap::Kind,
        perf_guest: u64,
        translate: impl FnOnce(&mut FunctionBuilder, codegen::Helpers, &mut dyn FnMut() -> u64),
    ) -> CompiledPtr {
        let started = Instant::now();
        let mut jit = self.inner.lock().unwrap();
        jit.next_id += 1;
        let name = format!("blk_{}", jit.next_id);

        let mut ctx = jit.module.make_context();
        let ptr = jit.module.target_config().pointer_type();
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.returns.push(AbiParam::new(types::I64));

        // Six Rust helpers, reached from compiled code by `call_indirect` through
        // their baked absolute address rather than a linker-relocated direct call —
        // so the emitted machine code carries no relocations (the prerequisite for a
        // persistable AOT code cache; see backlog/docs/design/aot-plan.md). Build each
        // signature here; `import_signature` + the fn address are wired into
        // `Helpers` below, inside the builder scope.
        // `n` I64 params, plus an I64 return when `ret` (the value-returning helpers);
        // the void helpers (cpuid/xgetbv/vmaskmov/bmi write through a `cpu`/`out` pointer).
        let params = |n: usize, ret: bool| {
            let mut s = jit.module.make_signature();
            for _ in 0..n {
                s.params.push(AbiParam::new(types::I64));
            }
            if ret {
                s.returns.push(AbiParam::new(types::I64));
            }
            s
        };
        let div_sig = params(6, true); // div(hi, lo, divisor, size, signed, out) -> i64
        let str_sig = params(8, true); // string(cpu, mem, op, elem, rep, cur_addr, addr_bits, seg_base) -> i64
        let x87_sig = params(6, true); // x87(cpu, mem, kind, addr, sti, cur_addr) -> i64
        let fx_sig = params(5, true); // fxstate(cpu, mem, addr, restore, cur_addr) -> i64
        let crc_sig = params(3, true); // crc32(crc, src, bytes) -> i64
        let note_watch_sig = params(3, false); // note_watch(mem_self, addr, len) -> ()
        let cpuid_sig = params(1, false); // cpuid(cpu) -> ()
        let xgetbv_sig = params(1, false); // xgetbv(cpu) -> ()
        let vmaskmov_sig = params(7, false); // vmaskmov(cpu, dst, src, k, elem, zeroing, bytes) -> ()
        let vmaskmov_mem_sig = params(10, true); // (cpu, mem, reg, addr, k, elem, zeroing, bytes, is_store, cur_addr) -> i64
        let vmasked_logic_sig = params(9, false); // (cpu, op, dst, a, b, k, elem, zeroing, bytes) -> ()
        let vmasked_packed_sig = params(9, false); // (cpu, op, dst, a, b, k, elem, zeroing, bytes) -> ()
        let vmasked_shift_sig = params(10, false); // (cpu, dst, a, imm, elem, right, arith, k, zeroing, bytes) -> ()
        let var_shift_sig = params(10, false); // (cpu, dst, a, count, elem, right, arith, k, zeroing, bytes) -> ()
        let shift_reg_sig = params(10, false); // (cpu, dst, a, count, elem, right, arith, k, zeroing, bytes) -> ()
        let gf2p8_sig = params(9, false); // (cpu, dst, a, b, imm, mode, k, zeroing, bytes) -> ()
        let gf2p8_mem_sig = params(10, true); // (cpu, mem, dst, a, addr, imm, mode, k, zeroing, bytes) -> ret
        let valign_sig = params(7, false); // valign(cpu, dst, a, b, shift, elem, bytes) -> ()
        let vpermt2_sig = params(10, false); // (cpu, dst, idx, tbl, elem, k, masked, zeroing, bytes, imode) -> ()
        let vpermt2_mem_sig = params(12, true); // (cpu, mem, dst, idx, addr, elem, k, masked, zeroing, bytes, imode, cur_addr) -> ret
        let vperm1_sig = params(9, false); // (cpu, dst, idx, src, elem, bytes, k, masked, zeroing) -> ()
        let vperm1_mem_sig = params(11, true); // (cpu, mem, dst, idx, addr, elem, k, masked, zeroing, bytes, cur_addr) -> ret
        let vpmov_narrow_sig = params(9, false); // (cpu, dst, src, from, to, src_width, k, masked, zeroing) -> ()
        let vpmov_narrow_mem_sig = params(8, true); // (cpu, mem, src, addr, from, to, src_width, cur_addr) -> ret
        let vpmov_extend_wide_sig = params(10, false); // (cpu, dst, src, from, to, signed, dst_width, k, masked, zeroing) -> ()
        let vpabs_sig = params(8, false); // (cpu, dst, src, elem, dst_width, k, masked, zeroing) -> ()
        let vp_unary_lane_sig = params(10, false); // (cpu, dst, src, op, imm, elem, dst_width, k, masked, zeroing) -> ()
        let vp_blendm_sig = params(8, false); // (cpu, dst, a, b, k, elem, dst_width, zeroing) -> ()
        let vshuf_lane_sig = params(10, false); // (cpu, dst, a, b, imm, elem, dst_width, k, masked, zeroing) -> ()
        let vp_multishift_sig = params(8, false); // (cpu, dst, ctrl, data, dst_width, k, masked, zeroing) -> ()
        let vpshufb_wide_sig = params(8, false); // (cpu, dst, a, idx, bytes, k, masked, zeroing) -> ()
        let vshuffle32_wide_sig = params(8, false); // (cpu, dst, a, imm, bytes, k, masked, zeroing) -> ()
        let vpack_sig = params(7, false); // (cpu, dst, a, b, from_elem, signed, bytes) -> ()
        let vpack_mem_sig = params(6, false); // (cpu, dst, lo, hi, from_elem, signed) -> ()
        let vhfloat_sig = params(6, false); // (cpu, dst, a, b, op, f64) -> ()
        let vhfloat_mem_sig = params(6, false); // (cpu, dst, lo, hi, op, f64) -> ()
        let vhint_sig = params(5, false); // (cpu, dst, a, b, op) -> ()
        let vhint_mem_sig = params(5, false); // (cpu, dst, lo, hi, op) -> ()
        let pmaddwd_sig = params(4, false); // (cpu, dst, a, b) -> ()
        let fma_sig = params(13, false); // (cpu, dst, x, y, z, prec_f64, scalar, neg_prod, neg_add, bytes) -> ()
        let fma_mem_sig = params(17, true); // (cpu, mem, dst, x, y, z, base, mem_role, prec_f64, scalar, neg_prod, neg_add, bytes, cur_addr) -> ret
        let broadcast_lane_sig = params(9, false); // (cpu,dst,src,chunk,elem,dst_width,k,masked,zeroing)
        let broadcast_lane_mem_sig = params(11, true); // (cpu,mem,dst,base,chunk,elem,dst_width,k,masked,zeroing,cur)
        let aes_sig = params(6, false); // aes(cpu, dst, a, b, op, imm) -> ()
        let aes_mem_sig = params(7, false); // aes_mem(cpu, dst, a, lo, hi, op, imm) -> ()
        let sha_sig = params(6, false); // sha(cpu, dst, a, b, op, imm) -> ()
        let sha_mem_sig = params(7, false); // sha_mem(cpu, dst, a, lo, hi, op, imm) -> ()
        let gfni_sig = params(6, false); // gfni(cpu, dst, a, b, op, imm) -> ()
        let gfni_mem_sig = params(7, false); // gfni_mem(cpu, dst, a, lo, hi, op, imm) -> ()
        let pclmul_sig = params(5, false); // pclmul(cpu, dst, a, b, imm) -> ()
        let pclmul_mem_sig = params(6, false); // pclmul_mem(cpu, dst, a, lo, hi, imm) -> ()
        let mmx_bridge_sig = params(4, false); // mmx_bridge(cpu, op, a, b) -> ()
        let pcmpstr_sig = params(6, false); // pcmpstr(cpu, a, b, imm, explicit, out) -> ()
        let pcmpstr_mem_sig = params(7, false); // pcmpstr_mem(cpu, a, bv_lo, bv_hi, imm, explicit, out) -> ()
        let pcmpstrm_sig = params(6, false); // pcmpstrm(cpu, a, b, imm, explicit, out) -> ()
        let pcmpstrm_mem_sig = params(7, false); // pcmpstrm_mem(cpu, a, bv_lo, bv_hi, imm, explicit, out) -> ()
        let bmi_sig = params(5, false); // bmi(a, b, op, size, out) -> () — result + CF via `out`

        {
            let Jit { fbctx, slots, .. } = &mut *jit;
            let mut alloc_slot = || {
                let b = Box::new(AtomicU64::new(0));
                let addr = &*b as *const AtomicU64 as u64;
                slots.push(b);
                addr
            };
            let mut builder = FunctionBuilder::new(&mut ctx.func, fbctx);
            // Each helper is `(imported signature ref, baked fn address)`.
            macro_rules! helper {
                ($sig:expr, $f:expr) => {
                    (builder.import_signature($sig), $f as *const u8 as u64)
                };
            }
            let helpers = codegen::Helpers {
                div: helper!(div_sig, div_helper),
                string: helper!(str_sig, string_helper),
                cpuid: helper!(cpuid_sig, cpuid_helper),
                xgetbv: helper!(xgetbv_sig, xgetbv_helper),
                vmaskmov: helper!(vmaskmov_sig, vmaskmov_helper),
                vmaskmov_mem: helper!(vmaskmov_mem_sig, vmaskmov_mem_helper),
                vmasked_logic: helper!(vmasked_logic_sig, vmasked_logic_helper),
                vmasked_packed: helper!(vmasked_packed_sig, vmasked_packed_helper),
                vmasked_shift: helper!(vmasked_shift_sig, vmasked_shift_helper),
                var_shift: helper!(var_shift_sig, var_shift_helper),
                shift_reg: helper!(shift_reg_sig, shift_reg_helper),
                gf2p8: helper!(gf2p8_sig, gf2p8_helper),
                gf2p8_mem: helper!(gf2p8_mem_sig, gf2p8_mem_helper),
                valign: helper!(valign_sig, valign_helper),
                vpermt2: helper!(vpermt2_sig, vpermt2_helper),
                vpermt2_mem: helper!(vpermt2_mem_sig, vpermt2_mem_helper),
                vperm1: helper!(vperm1_sig, vperm1_helper),
                vperm1_mem: helper!(vperm1_mem_sig, vperm1_mem_helper),
                vpmov_narrow: helper!(vpmov_narrow_sig, vpmov_narrow_helper),
                vpmov_narrow_mem: helper!(vpmov_narrow_mem_sig, vpmov_narrow_mem_helper),
                vpmov_extend_wide: helper!(vpmov_extend_wide_sig, vpmov_extend_wide_helper),
                vpabs: helper!(vpabs_sig, vpabs_helper),
                vp_unary_lane: helper!(vp_unary_lane_sig, vp_unary_lane_helper),
                vp_blendm: helper!(vp_blendm_sig, vp_blendm_helper),
                vshuf_lane: helper!(vshuf_lane_sig, vshuf_lane_helper),
                vp_multishift: helper!(vp_multishift_sig, vp_multishift_helper),
                vpshufb_wide: helper!(vpshufb_wide_sig, vpshufb_wide_helper),
                vshuffle32_wide: helper!(vshuffle32_wide_sig, vshuffle32_wide_helper),
                vpack: helper!(vpack_sig, vpack_helper),
                vpack_mem: helper!(vpack_mem_sig, vpack_mem_helper),
                vhfloat: helper!(vhfloat_sig, vhfloat_helper),
                vhfloat_mem: helper!(vhfloat_mem_sig, vhfloat_mem_helper),
                vhint: helper!(vhint_sig, vhint_helper),
                vhint_mem: helper!(vhint_mem_sig, vhint_mem_helper),
                pmaddwd: helper!(pmaddwd_sig, pmaddwd_helper),
                fma: helper!(fma_sig, fma_helper),
                fma_mem: helper!(fma_mem_sig, fma_mem_helper),
                broadcast_lane: helper!(broadcast_lane_sig, broadcast_lane_helper),
                broadcast_lane_mem: helper!(broadcast_lane_mem_sig, broadcast_lane_mem_helper),
                aes: helper!(aes_sig, aes_helper),
                aes_mem: helper!(aes_mem_sig, aes_mem_helper),
                sha: helper!(sha_sig, sha_helper),
                sha_mem: helper!(sha_mem_sig, sha_mem_helper),
                gfni: helper!(gfni_sig, gfni_helper),
                gfni_mem: helper!(gfni_mem_sig, gfni_mem_helper),
                pclmul: helper!(pclmul_sig, pclmul_helper),
                pclmul_mem: helper!(pclmul_mem_sig, pclmul_mem_helper),
                mmx_bridge: helper!(mmx_bridge_sig, mmx_bridge_helper),
                pcmpstr: helper!(pcmpstr_sig, pcmpstr_helper),
                pcmpstr_mem: helper!(pcmpstr_mem_sig, pcmpstr_mem_helper),
                pcmpstrm: helper!(pcmpstrm_sig, pcmpstrm_helper),
                pcmpstrm_mem: helper!(pcmpstrm_mem_sig, pcmpstrm_mem_helper),
                bmi: helper!(bmi_sig, bmi_helper),
                x87: helper!(x87_sig, x87_helper),
                fxstate: helper!(fx_sig, fxstate_helper),
                crc32: helper!(crc_sig, crc32_helper),
                note_watch: helper!(note_watch_sig, note_watched_write_helper),
            };
            translate(&mut builder, helpers, &mut alloc_slot);
            builder.finalize();
        }

        let id = jit
            .module
            .declare_function(&name, Linkage::Export, &ctx.func.signature)
            .expect("declare function");
        jit.module
            .define_function(id, &mut ctx)
            .expect("define function");
        // GP-3 (doc-30): capture the code size + sorted `(host_off, guest_rip)`
        // srcloc table before `clear_context` wipes it, to register in the
        // process-global `CodeMap` once the host entry address is known below.
        let (code_len, srcloc_table) = {
            let cc = ctx.compiled_code().expect("compiled code present");
            let table: Vec<(u32, u32)> = cc
                .buffer
                .get_srclocs_sorted()
                .iter()
                .filter(|s| !s.loc.is_default())
                .map(|s| (s.start, s.loc.bits()))
                .collect();
            (cc.code_info().total_size, table.into_boxed_slice())
        };
        jit.module.clear_context(&mut ctx);
        jit.module.finalize_definitions().expect("finalize");

        let entry = CompiledPtr(jit.module.get_finalized_function(id));
        x86jit_core::codemap::register(entry.0 as usize, code_len, srcloc_table);
        // task-196: mirror this range into `/tmp/perf-<pid>.map` iff X86JIT_PERF_MAP=1
        // (no-op otherwise). Same host range as `codemap`, named by the guest RIP so
        // `perf` attributes samples in JIT'd code to `jit_0x<guest_rip>`. The guest PC
        // is the real block/region entry, not the srcloc table's first entry (whose
        // guest RIPs are truncated to u32).
        perfmap::record(entry.0 as usize, code_len, perf_kind, perf_guest);
        // Account this compile's wall-time for the bench compile-vs-run split (PB-2).
        // Includes the `inner` lock wait — that contention is real compile-path cost.
        self.compile_ns
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        entry
    }

    /// Compile one background request's unit (bg-tier, doc-27 D3): a single block, or
    /// (BGT-6) a hotness-gated superblock region — same off-thread path either way.
    fn compile_request(&self, req: &TierUpRequest) -> CompiledPtr {
        match &req.unit {
            TierUpUnit::Block(ir) => self.compile(ir, req.consistency, req.mmio, req.guest_base),
            TierUpUnit::Region(region) => {
                self.compile_region(region, req.consistency, req.mmio, req.guest_base)
            }
        }
    }

    /// The background compiler loop (bg-tier, doc-27 D3): pull a request, compile it
    /// under the shared JIT mutex (so `JITModule`'s `!Sync`/`&mut finalize` is
    /// satisfied exactly as the foreground path, serialized against it), publish the
    /// result to `done`, and repeat until `Drop` sets `shutdown`.
    fn worker_loop(&self) {
        loop {
            let req = {
                let mut q = self.queue.lock().unwrap();
                // Park while empty or paused; `shutdown` always wins so `Drop` joins.
                while (q.items.is_empty() || q.paused) && !q.shutdown {
                    q = self.work_cv.wait(q).unwrap();
                }
                if q.shutdown && q.items.is_empty() {
                    break;
                }
                q.items.pop_front().expect("non-empty queue")
            };
            // Compile OUTSIDE the queue lock (it takes `inner`); a concurrent submit
            // or foreground `materialize` isn't blocked by this block's compile.
            let entry = self.compile_request(&req);
            {
                let mut done = self.done.lock().unwrap();
                done.push(TierUpFinished {
                    pc: req.pc,
                    block: CachedBlock::Compiled { entry },
                    spans: req.spans.clone(),
                    epoch: req.epoch,
                });
                // Keep the lock-free probe equal to `done.len()` (set under the lock).
                self.ready.store(done.len(), Ordering::Release);
            }
            let mut q = self.queue.lock().unwrap();
            q.outstanding -= 1;
            if q.outstanding == 0 {
                self.idle_cv.notify_all();
            }
        }
    }
}

/// A cloneable handle to a [`JitBackend`]'s background tier-up machinery (bg-tier,
/// doc-27 D6), exposing `wait_idle` for deterministic tests without owning the
/// worker thread.
pub struct TierUpHandle {
    shared: Arc<Shared>,
}

impl TierUpHandle {
    /// Block until every submitted background compile has completed and its result
    /// is queued for drain (the completions are then observable via
    /// `Backend::tier_up_finished`). No sleeps — waits on the worker's idle signal.
    pub fn wait_idle(&self) {
        let mut q = self.shared.queue.lock().unwrap();
        while q.outstanding > 0 {
            q = self.shared.idle_cv.wait(q).unwrap();
        }
    }

    /// Test lever (bg-tier BGT-4): park the background worker so queued requests pile
    /// up uncompiled until the returned guard drops — lets a race test line up several
    /// in-flight requests for one pc before any of them lands. Sets a queue flag (it
    /// does NOT hold the compiler mutex, so a vcpu can still invalidate/`materialize`
    /// on the same thread). Not for production use.
    pub fn pause_compiler(&self) -> CompilerPause {
        self.shared.queue.lock().unwrap().paused = true;
        CompilerPause {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Guard from [`TierUpHandle::pause_compiler`]: releasing it unparks the worker.
pub struct CompilerPause {
    shared: Arc<Shared>,
}

impl Drop for CompilerPause {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().paused = false;
        self.shared.work_cv.notify_all();
    }
}

impl Default for JitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for JitBackend {
    fn materialize(
        &self,
        ir: &IrBlock,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CachedBlock {
        CachedBlock::Compiled {
            entry: self.shared.compile(ir, consistency, mmio, guest_base),
        }
    }

    fn region_caps(&self) -> Option<RegionCaps> {
        self.shared.caps
    }

    fn materialize_region(
        &self,
        region: &IrRegion,
        consistency: MemConsistency,
        mmio: Option<(u64, u64)>,
        guest_base: u64,
    ) -> CachedBlock {
        CachedBlock::Compiled {
            entry: self
                .shared
                .compile_region(region, consistency, mmio, guest_base),
        }
    }

    fn invalidate_links(&self) {
        // Zero every link slot so no surviving block chains into a unit an SMC
        // invalidation just dropped (R1). Over-invalidation (all slots, not only
        // the victims') is deliberate: invalidation is rare, and a cleared slot
        // simply re-links via `RET_LINK` on its next traversal. Relaxed stores pair
        // with the dispatcher's relaxed fill; compiled-code reads see 0 or a valid
        // entry. Runs under the compiler mutex, off the hot path.
        let jit = self.shared.inner.lock().unwrap();
        for slot in &jit.slots {
            slot.store(0, Ordering::Relaxed);
        }
    }

    fn tier_up_async(&self, req: TierUpRequest) -> TierUpSubmit {
        self.ensure_worker();
        let mut q = self.shared.queue.lock().unwrap();
        if q.shutdown {
            // Racing `Drop` — decline; the caller stays interpreted (correct, slow).
            return TierUpSubmit::Unsupported;
        }
        if q.items.len() >= TIER_QUEUE_CAP {
            // Backpressure: never compile inline in response (doc-27 D1).
            return TierUpSubmit::Busy;
        }
        q.items.push_back(req);
        q.outstanding += 1;
        drop(q);
        self.shared.work_cv.notify_one();
        TierUpSubmit::Queued
    }

    fn tier_up_finished(&self) -> Vec<TierUpFinished> {
        // Fast path: nothing published since the last drain — no lock, no alloc.
        if self.shared.ready.load(Ordering::Acquire) == 0 {
            return Vec::new();
        }
        let mut done = self.shared.done.lock().unwrap();
        self.shared.ready.store(0, Ordering::Release);
        std::mem::take(&mut *done)
    }

    fn compile_ns(&self) -> u64 {
        self.shared.compile_ns.load(Ordering::Relaxed)
    }
}

impl Drop for JitBackend {
    fn drop(&mut self) {
        // Signal shutdown and wake the worker so no thread outlives the module it
        // compiles into (use-after-free guard). A worker that panicked poisons the
        // queue mutex — recover the guard and still join; never re-panic in `Drop`
        // (a dead worker just means blocks stay interpreted: slow, not unsound).
        {
            let mut q = self.shared.queue.lock().unwrap_or_else(|p| p.into_inner());
            q.shutdown = true;
        }
        self.shared.work_cv.notify_all();
        if let Some(handle) = self
            .worker
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = handle.join(); // swallow a worker panic
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86jit_core::jit_abi::run_compiled;
    use x86jit_core::lift::{lift_block, CpuMode};
    use x86jit_core::{CpuState, Memory, MemoryModel, Prot, RegionKind, StepResult};

    // `mov eax, 42` then `jmp $` (to self): sets RAX=42 and terminates the block
    // without touching guest memory (the jmp is the block terminator, not a loop).
    const CODE: &[u8] = &[0xb8, 0x2a, 0x00, 0x00, 0x00, 0xeb, 0xf9];
    const ENTRY: u64 = 0x1000;

    fn mem_with_code() -> Memory {
        let mut m = Memory::new(MemoryModel::Flat { size: 0x4000 });
        m.map(ENTRY, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
        m.write_bytes(ENTRY, CODE).unwrap();
        m
    }

    fn request(mem: &Memory) -> TierUpRequest {
        let ir = lift_block(mem, ENTRY, CpuMode::Long64).expect("lift the block");
        TierUpRequest {
            pc: ENTRY,
            unit: TierUpUnit::Block(Arc::new(ir)),
            consistency: MemConsistency::Fast,
            mmio: None,
            guest_base: 0,
            spans: vec![(ENTRY, CODE.len() as u32)],
            epoch: 0,
        }
    }

    /// Run a compiled block from a fresh CPU and return RAX (should be 42).
    fn run_rax(entry: CompiledPtr, mem: &Memory) -> u64 {
        let mut cpu = CpuState::new();
        cpu.rip = ENTRY;
        let step = unsafe { run_compiled(entry, &mut cpu, mem, CpuMode::Long64) };
        assert!(matches!(step, StepResult::Continue), "block continues");
        cpu.gpr[0]
    }

    fn compiled_entry(block: CachedBlock) -> CompiledPtr {
        match block {
            CachedBlock::Compiled { entry } => {
                assert!(!entry.0.is_null(), "compiled entry is non-null");
                entry
            }
            CachedBlock::Interpreted(_) => panic!("expected a compiled block"),
        }
    }

    /// AC#1: a background-compiled block is drained as a `Compiled` unit and runs
    /// correctly (RAX=42), matching the eager path.
    #[test]
    fn background_compile_runs_correctly() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let handle = jit.tier_up_handle();

        assert_eq!(jit.tier_up_async(request(&mem)), TierUpSubmit::Queued);
        handle.wait_idle();

        let mut done = jit.tier_up_finished();
        assert_eq!(done.len(), 1, "exactly one completion drained");
        let fin = done.pop().unwrap();
        assert_eq!(fin.pc, ENTRY);
        assert_eq!(fin.spans, vec![(ENTRY, CODE.len() as u32)]);
        assert_eq!(fin.epoch, 0);
        assert_eq!(run_rax(compiled_entry(fin.block), &mem), 42);

        // Drained: the fast probe is clear and a second drain is empty.
        assert!(jit.tier_up_finished().is_empty());
    }

    /// AC#5: no worker thread is spawned until the first `tier_up_async`.
    #[test]
    fn worker_spawns_lazily() {
        let jit = JitBackend::new();
        assert!(
            jit.worker.lock().unwrap().is_none(),
            "no thread before any tier-up request"
        );
        let mem = mem_with_code();
        let _ = jit.tier_up_async(request(&mem));
        assert!(
            jit.worker.lock().unwrap().is_some(),
            "the first request spawns the worker"
        );
    }

    /// AC#2: a full queue returns `Busy` (never an inline compile), and the queued
    /// requests all complete once the compiler is unblocked. The worker is stalled
    /// by holding the JIT mutex it needs, so the queue provably fills.
    #[test]
    fn busy_on_full_queue_then_queued_complete() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let handle = jit.tier_up_handle();

        let guard = jit.shared.inner.lock().unwrap(); // stall every compile
        let mut queued = 0usize;
        let mut busy = false;
        for _ in 0..(TIER_QUEUE_CAP + 8) {
            match jit.tier_up_async(request(&mem)) {
                TierUpSubmit::Queued => queued += 1,
                TierUpSubmit::Busy => busy = true,
                TierUpSubmit::Unsupported => panic!("unexpected Unsupported"),
            }
        }
        assert!(busy, "a full queue must report Busy");
        assert!(queued >= TIER_QUEUE_CAP, "queue filled to capacity");

        drop(guard); // unblock the worker
        handle.wait_idle();
        assert_eq!(
            jit.tier_up_finished().len(),
            queued,
            "every queued request completed"
        );
    }

    /// AC#4: the eager/foreground `materialize` still works (correct output, no
    /// deadlock) while the worker churns a backlog — both take the same JIT mutex.
    #[test]
    fn eager_materialize_works_while_worker_busy() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let handle = jit.tier_up_handle();

        for _ in 0..30 {
            let _ = jit.tier_up_async(request(&mem));
        }
        // Foreground compile the same block amid the backlog.
        let ir = lift_block(&mem, ENTRY, CpuMode::Long64).unwrap();
        let entry = compiled_entry(jit.materialize(&ir, MemConsistency::Fast, None, 0));
        assert_eq!(run_rax(entry, &mem), 42);

        handle.wait_idle();
        for fin in jit.tier_up_finished() {
            assert_eq!(run_rax(compiled_entry(fin.block), &mem), 42);
        }
    }

    /// task-196 AC#2: the host range a compiled block emits to the perf map starts
    /// at the same host entry `codemap` recorded (both take `entry.0` at the same
    /// call site), and the block is genuinely covered by `codemap` at that guest
    /// RIP. This intentionally does NOT assert on `codemap::lookup(entry.0)`: at the
    /// exact entry offset the srcloc table may have no covering entry (returns
    /// `None`), and its guest RIPs are u32-truncated — which is *why* perfmap names
    /// the symbol from `ir.guest_start` directly, not from codemap. Instead we scan
    /// forward within the block and confirm codemap resolves it to `ENTRY` (which
    /// fits in u32 here), proving the perf-map range and codemap agree on the block.
    /// (Line formatting is covered by `perfmap::tests::format_line_*`; end-to-end
    /// file emission by the bench under `X86JIT_PERF_MAP=1`.)
    #[test]
    fn perfmap_range_matches_codemap() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let ir = lift_block(&mem, ENTRY, CpuMode::Long64).unwrap();
        let entry = compiled_entry(jit.materialize(&ir, MemConsistency::Fast, None, 0));
        let start = entry.0 as usize;
        // Some host offset inside the compiled block must resolve, via codemap, to
        // the same guest RIP (ENTRY) the perf-map symbol `jit_0x1000` names. Scan a
        // small window — the first srcloc need not be at offset 0.
        let hit = (0..64).find_map(|off| x86jit_core::codemap::lookup(start + off));
        assert_eq!(
            hit,
            Some(ENTRY),
            "codemap covers the compiled block at the guest RIP perfmap names it by"
        );
    }

    /// AC#3a: dropping with requests queued/mid-compile joins the worker cleanly
    /// (the test hangs on a leaked thread, panics on a double-free — neither here).
    #[test]
    fn drop_joins_with_work_queued() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        for _ in 0..20 {
            let _ = jit.tier_up_async(request(&mem));
        }
        drop(jit); // must join without hanging
    }

    /// AC#3b: a poisoned queue mutex (a stand-in for a panicked worker) must not
    /// make `Drop` re-panic — a dead worker only means blocks stay interpreted.
    #[test]
    fn drop_survives_poisoned_mutex() {
        let mem = mem_with_code();
        let jit = JitBackend::new();
        let _ = jit.tier_up_async(request(&mem)); // spawn the worker
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = jit.shared.queue.lock().unwrap();
            panic!("poison the queue mutex");
        }));
        drop(jit); // must not re-panic despite the poisoned mutex
    }
}
