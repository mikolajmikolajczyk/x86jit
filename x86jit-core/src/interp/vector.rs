//! Extracted `interpret_block` dispatch arm bodies (vector); see `super`.

use super::*;
use crate::ir::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_load(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    size: &u8,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    match vload(mem, a, *size) {
        Ok(v) => cpu.xmm[*dst as usize] = v,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, *size, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_store(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    addr: &Val,
    src: &u8,
    size: &u8,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let v = cpu.xmm[*src as usize];
    if let Err(t) = vstore(mem, a, v, *size) {
        return Some(trap_out(
            cpu,
            cur_addr,
            t,
            a,
            *size,
            AccessKind::Write,
            v as u64,
        ));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_mov(cpu: &mut CpuState, dst: &u8, src: &u8) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = cpu.xmm[*src as usize];
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_load_wide(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    bytes: &u16,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let mut lanes = [0u128; 4];
    // Load `bytes/16` 128-bit lanes; set_vec zero-extends above `bytes`.
    for (i, slot) in lanes.iter_mut().enumerate().take(*bytes as usize / 16) {
        let ea = a.wrapping_add(i as u64 * 16);
        match vload(mem, ea, 16) {
            Ok(v) => *slot = v,
            Err(t) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
        }
    }
    cpu.set_vec(*dst as usize, lanes, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_store_wide(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    addr: &Val,
    src: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let lanes = cpu.vec_lanes(*src as usize);
    for (i, v) in lanes.into_iter().enumerate().take(*bytes as usize / 16) {
        let ea = a.wrapping_add(i as u64 * 16);
        if let Err(t) = vstore(mem, ea, v, 16) {
            return Some(trap_out(
                cpu,
                cur_addr,
                t,
                ea,
                16,
                AccessKind::Write,
                v as u64,
            ));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_mov_wide(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let lanes = cpu.vec_lanes(*src as usize);
    cpu.set_vec(*dst as usize, lanes, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_mask_mov(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    k: &u8,
    elem: &u8,
    zeroing: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    let newval = cpu.vec_lanes(*src as usize);
    cpu.write_masked(*dst as usize, newval, *k, *elem, *zeroing, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_mask_load_mem(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    k: &u8,
    elem: &u8,
    zeroing: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let km = cpu.kmask[*k as usize];
    if let Some(f) = masked_load_run(cpu, mem, *dst, base, km, *elem, *zeroing, *bytes, cur_addr) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_mask_store_mem(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    src: &u8,
    addr: &Val,
    k: &u8,
    elem: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let km = cpu.kmask[*k as usize];
    if let Some(f) = masked_store_run(cpu, mem, *src, base, km, *elem, *bytes, cur_addr) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_vecmask_load_mem(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    mask: &u8,
    elem: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let km = vec_msb_mask(&cpu.vec_lanes(*mask as usize), *elem, *bytes);
    // AVX load form zeroes masked-off lanes (zeroing = true).
    if let Some(f) = masked_load_run(cpu, mem, *dst, base, km, *elem, true, *bytes, cur_addr) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_vecmask_store_mem(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    src: &u8,
    addr: &Val,
    mask: &u8,
    elem: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let km = vec_msb_mask(&cpu.vec_lanes(*mask as usize), *elem, *bytes);
    if let Some(f) = masked_store_run(cpu, mem, *src, base, km, *elem, *bytes, cur_addr) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_logic256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &VLogicOp,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = vlogic(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *op);
    cpu.ymm_hi[*dst as usize] = vlogic(cpu.ymm_hi[*a as usize], cpu.ymm_hi[*b as usize], *op);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_logic_wide(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &VLogicOp,
    bytes: &u16,
) -> Option<StepResult> {
    let (al, bl) = (cpu.vec_lanes(*a as usize), cpu.vec_lanes(*b as usize));
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = vlogic(al[i], bl[i], *op);
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_logic_wide_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    op: &VLogicOp,
    bytes: &u16,
) -> Option<StepResult> {
    let al = cpu.vec_lanes(*a as usize);
    let base = read_val(*addr, &*temps);
    let bl = match vload_lanes(mem, base, *bytes) {
        Ok(v) => v,
        Err((ea, t)) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    };
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = vlogic(al[i], bl[i], *op);
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_popcnt(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    lane: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let al = cpu.vec_lanes(*a as usize);
    let r = vpopcnt_lanes(al, *lane);
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_popcnt_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    lane: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let al = match vload_lanes(mem, base, *bytes) {
        Ok(v) => v,
        Err((ea, t)) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    };
    let r = vpopcnt_lanes(al, *lane);
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_mov_extend(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    from: &u8,
    to: &u8,
    signed: &bool,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = pmov_extend(cpu.xmm[*src as usize], *from, *to, *signed);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_mov_extend_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    from: &u8,
    to: &u8,
    signed: &bool,
) -> Option<StepResult> {
    let nbytes = (16 / *to as usize) * *from as usize;
    let av = read_val(*addr, &*temps);
    match vload(mem, av, nbytes as u8) {
        Ok(m) => cpu.xmm[*dst as usize] = pmov_extend(m, *from, *to, *signed),
        Err(t) => {
            return Some(trap_out(
                cpu,
                cur_addr,
                t,
                av,
                nbytes as u8,
                AccessKind::Read,
                0,
            ))
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_mov_extend_wide(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    from: &u8,
    to: &u8,
    signed: &bool,
    dst_width: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vpmov_extend_wide(
        cpu,
        *dst,
        *src,
        *from,
        *to,
        *signed,
        *dst_width,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_abs(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    elem: &u8,
    dst_width: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vpabs(
        cpu,
        *dst,
        *src,
        *elem,
        *dst_width,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_unary_lane(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    op: &VpUnaryOp,
    imm: &u8,
    elem: &u8,
    dst_width: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vp_unary_lane(
        cpu,
        *dst,
        *src,
        *op,
        *imm,
        *elem,
        *dst_width,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_blendm(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    k: &u8,
    elem: &u8,
    dst_width: &u16,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vp_blendm(cpu, *dst, *a, *b, *k, *elem, *dst_width, *zeroing);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_shuf_lane(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
    elem: &u8,
    dst_width: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vshuf_lane(
        cpu,
        *dst,
        *a,
        *b,
        *imm,
        *elem,
        *dst_width,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_multishift(
    cpu: &mut CpuState,
    dst: &u8,
    ctrl: &u8,
    data: &u8,
    dst_width: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vp_multishift(
        cpu,
        *dst,
        *ctrl,
        *data,
        *dst_width,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_blend_v(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    lane: &u8,
) -> Option<StepResult> {
    let (d, s, m) = (cpu.xmm[*dst as usize], cpu.xmm[*src as usize], cpu.xmm[0]);
    cpu.xmm[*dst as usize] = blendv(d, s, m, *lane);
    None
}

pub(crate) fn exec_v_p_blend_v_x(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    mask: &u8,
    lane: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    // Each 128-bit lane blends independently under its own mask lane (task-262).
    let av = cpu.vec_lanes(*a as usize);
    let bv = cpu.vec_lanes(*b as usize);
    let m = cpu.vec_lanes(*mask as usize);
    let mut r = [0u128; 4];
    for c in 0..(*bytes as usize / 16) {
        r[c] = blendv(av[c], bv[c], m[c], *lane);
    }
    cpu.set_vec(*dst as usize, r, *bytes); // clears bits above `bytes`
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_blend_v_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    lane: &u8,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let (d, m) = (cpu.xmm[*dst as usize], cpu.xmm[0]);
    match vload(mem, av, 16) {
        Ok(s) => cpu.xmm[*dst as usize] = blendv(d, s, m, *lane),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    }
    None
}

/// AVX `vblendv{ps,pd}`/`vpblendvb` with an m128/m256 src2 (task-256/262): the m128 form is
/// the exact Celeste wall. Read `a` (src1) and the `mask` register before writing `dst` so
/// either aliasing `dst` is safe; a fault on the load traps. Each 128-bit lane blends
/// independently; `set_vec` clears bits above `bytes`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_blend_v_xm(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    mask: &u8,
    lane: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let av = cpu.vec_lanes(*a as usize);
    let m = cpu.vec_lanes(*mask as usize);
    let addr_v = read_val(*addr, &*temps);
    let bv = match vload_lanes(mem, addr_v, *bytes) {
        Ok(v) => v,
        Err((ea, t)) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    };
    let mut r = [0u128; 4];
    for c in 0..(*bytes as usize / 16) {
        r[c] = blendv(av[c], bv[c], m[c], *lane);
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

/// SSE/AVX imm8 static blend `blendps`/`blendpd`/`vblend*` register src2 (task-256/262). Read
/// `a` (merge base) and `b` before writing `dst` so aliasing is safe. For the ymm form the
/// imm8 covers up to 8 lanes across both halves (high lane uses the shifted imm bits);
/// `set_vec` clears bits above `bytes`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_blend_i(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
    lane: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let av = cpu.vec_lanes(*a as usize);
    let bv = cpu.vec_lanes(*b as usize);
    let mut r = [0u128; 4];
    let per_half = 16 / *lane; // imm bits consumed by each 128-bit lane
    for c in 0..(*bytes as usize / 16) {
        r[c] = blendi(av[c], bv[c], imm >> (c as u8 * per_half), *lane);
    }
    cpu.set_vec_low(*dst as usize, r, *bytes); // SSE preserves upper; VEX.128 zeroes via VZeroUpper
    None
}

/// As [`exec_v_blend_i`] but src2 is an m128/m256 memory operand (task-256/262). `a` is read
/// before `dst` is written so `a` aliasing `dst` is safe; a fault on the load traps.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_blend_i_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm: &u8,
    lane: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let av = cpu.vec_lanes(*a as usize); // read merge base before dst is written
    let addr_v = read_val(*addr, &*temps);
    let bv = match vload_lanes(mem, addr_v, *bytes) {
        Ok(v) => v,
        Err((ea, t)) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    };
    let mut r = [0u128; 4];
    let per_half = 16 / *lane;
    for c in 0..(*bytes as usize / 16) {
        r[c] = blendi(av[c], bv[c], imm >> (c as u8 * per_half), *lane);
    }
    cpu.set_vec_low(*dst as usize, r, *bytes); // SSE preserves upper; VEX.128 zeroes via VZeroUpper
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_round(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    src: &u8,
    prec: &FPrec,
    mode: &u8,
    scalar: &bool,
) -> Option<StepResult> {
    let (d, s) = (cpu.xmm[*a as usize], cpu.xmm[*src as usize]);
    cpu.xmm[*dst as usize] = vround(d, s, *prec, *mode, *scalar);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_round_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    prec: &FPrec,
    mode: &u8,
    scalar: &bool,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    // Packed loads 16 bytes; scalar loads only one element.
    let size = if *scalar { prec.bytes() } else { 16 };
    let d = cpu.xmm[*dst as usize];
    match vload(mem, av, size) {
        Ok(s) => cpu.xmm[*dst as usize] = vround(d, s, *prec, *mode, *scalar),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, size, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_masked_logic(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &VLogicOp,
    k: &u8,
    elem: &u8,
    zeroing: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    apply_masked_logic(cpu, *op, *dst, *a, *b, *k, *elem, *zeroing, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_masked_packed(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &PackedBinOp,
    k: &u8,
    elem: &u8,
    zeroing: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    apply_masked_packed(cpu, *op, *dst, *a, *b, *k, *elem, *zeroing, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert_lane_wide(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    ins: &u8,
    idx: &u8,
    num_lanes: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let mut lanes = cpu.vec_lanes(*src as usize);
    let inl = cpu.vec_lanes(*ins as usize);
    let base = *idx as usize * *num_lanes as usize;
    let n = *num_lanes as usize;
    lanes[base..base + n].copy_from_slice(&inl[..n]);
    cpu.set_vec(*dst as usize, lanes, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_extract_lane_wide(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    idx: &u8,
    num_lanes: &u8,
) -> Option<StepResult> {
    let srcl = cpu.vec_lanes(*src as usize);
    let n = *num_lanes as usize;
    let base = *idx as usize * n;
    let mut out = [0u128; 4];
    out[..n].copy_from_slice(&srcl[base..base + n]);
    cpu.set_vec(*dst as usize, out, (n as u16) * 16);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_extract_lane_wide_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    src: &u8,
    addr: &Val,
    idx: &u8,
    num_lanes: &u8,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let srcl = cpu.vec_lanes(*src as usize);
    let n = *num_lanes as usize;
    let base = *idx as usize * n;
    // Atomic whole-region semantics, matching the JIT (task-219). A real CPU faults the
    // whole store atomically, committing nothing; the JIT's `emit_v_extract_lane_wide_m`
    // does one up-front `checked_addr(a, n*16, ..)` which — see `checked_addr` — reports
    // the fault at the BASE address `a` (it stores `MEMCTX_FAULT_ADDR = addr`) with size
    // `n*16` and writes nothing. So we pre-probe the entire `[a, a + n*16)` destination
    // before storing any lane; on any trap we surface it at `a` with size `n*16`, never
    // committing a partial store that would leak a different faulting address than the JIT.
    // A `vload` probe shares `region_at` + the `Trap`/unmapped checks with `vstore`, so it
    // faults on exactly the sub-addresses a store would (there are no read-only regions).
    for i in 0..n {
        let ea = a.wrapping_add(i as u64 * 16);
        if let Err(t) = vload(mem, ea, 16) {
            return Some(trap_out(
                cpu,
                cur_addr,
                t,
                a,
                (n * 16) as u8,
                AccessKind::Write,
                srcl[base] as u64, // low 8 bytes of lane 0, only used for an MMIO-write exit
            ));
        }
    }
    for (i, &v) in srcl[base..base + n].iter().enumerate() {
        let ea = a.wrapping_add(i as u64 * 16);
        // Probed writable above; a fault here would be an unmodeled cross-vcpu race.
        let _ = vstore(mem, ea, v, 16);
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pcmp_str(
    cpu: &mut CpuState,
    a: &u8,
    b: &u8,
    imm: &u8,
    explicit: &bool,
) -> Option<StepResult> {
    let (ecx, cf, zf, sf, of) = pcmpstr_run(cpu, *a, *b, *imm, *explicit);
    cpu.write_gpr(1, ecx as u64, 4); // ECX (zero-extends RCX)
    cpu.flags.cf = cf;
    cpu.flags.zf = zf;
    cpu.flags.sf = sf;
    cpu.flags.of = of;
    cpu.flags.af = false;
    cpu.flags.pf = false;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pcmp_str_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    a: &u8,
    addr: &Val,
    imm: &u8,
    explicit: &bool,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let bv = match vload(mem, av, 16) {
        Ok(v) => v,
        Err(t) => {
            return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0));
        }
    };
    let (ecx, cf, zf, sf, of) = pcmpstr_run_bv(cpu, *a, bv, *imm, *explicit);
    cpu.write_gpr(1, ecx as u64, 4); // ECX (zero-extends RCX)
    cpu.flags.cf = cf;
    cpu.flags.zf = zf;
    cpu.flags.sf = sf;
    cpu.flags.of = of;
    cpu.flags.af = false;
    cpu.flags.pf = false;
    None
}

/// Store the `pcmpstr` mask + flags (task-195). Shared by the register and memory arms.
fn write_pcmpstrm(cpu: &mut CpuState, mask: u128, cf: bool, zf: bool, sf: bool, of: bool) {
    cpu.xmm[0] = mask; // XMM0 (low 128; legacy SSE preserves 255:128)
    cpu.flags.cf = cf;
    cpu.flags.zf = zf;
    cpu.flags.sf = sf;
    cpu.flags.of = of;
    cpu.flags.af = false;
    cpu.flags.pf = false;
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pcmp_str_mask(
    cpu: &mut CpuState,
    a: &u8,
    b: &u8,
    imm: &u8,
    explicit: &bool,
) -> Option<StepResult> {
    let (mask, cf, zf, sf, of) = pcmpstrm_run(cpu, *a, *b, *imm, *explicit);
    write_pcmpstrm(cpu, mask, cf, zf, sf, of);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pcmp_str_mask_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    a: &u8,
    addr: &Val,
    imm: &u8,
    explicit: &bool,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let bv = match vload(mem, av, 16) {
        Ok(v) => v,
        Err(t) => {
            return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0));
        }
    };
    let (mask, cf, zf, sf, of) = pcmpstrm_run_bv(cpu, *a, bv, *imm, *explicit);
    write_pcmpstrm(cpu, mask, cf, zf, sf, of);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert_ps(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    imm: &u8,
) -> Option<StepResult> {
    let src_lane = ((*imm >> 6) & 3) as usize;
    let tmp = (cpu.xmm[*src as usize] >> (src_lane * 32)) as u32;
    cpu.xmm[*dst as usize] = insertps(cpu.xmm[*dst as usize], tmp, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert_ps_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    imm: &u8,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let tmp = match vload(mem, av, 4) {
        Ok(v) => v as u32,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 4, AccessKind::Read, 0)),
    };
    cpu.xmm[*dst as usize] = insertps(cpu.xmm[*dst as usize], tmp, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert_ps3(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    src: &u8,
    imm: &u8,
) -> Option<StepResult> {
    // Read both sources before writing dst so either aliasing dst is safe (VEX form).
    let src_lane = ((*imm >> 6) & 3) as usize;
    let tmp = (cpu.xmm[*src as usize] >> (src_lane * 32)) as u32;
    cpu.xmm[*dst as usize] = insertps(cpu.xmm[*a as usize], tmp, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert_ps_m3(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm: &u8,
) -> Option<StepResult> {
    let base = cpu.xmm[*a as usize]; // read merge base before dst is written
    let av = read_val(*addr, &*temps);
    let tmp = match vload(mem, av, 4) {
        Ok(v) => v as u32,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 4, AccessKind::Read, 0)),
    };
    cpu.xmm[*dst as usize] = insertps(base, tmp, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_dpps(cpu: &mut CpuState, dst: &u8, b: &u8, imm: &u8) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = dpps(cpu.xmm[*dst as usize], cpu.xmm[*b as usize], *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_dpps_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    imm: &u8,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let bv = match vload(mem, av, 16) {
        Ok(v) => v,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    };
    cpu.xmm[*dst as usize] = dpps(cpu.xmm[*dst as usize], bv, *imm);
    None
}

/// SSE4.1 `dppd` (task-256): double-precision dot product. `dst` is also source 1.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_dppd(cpu: &mut CpuState, dst: &u8, b: &u8, imm: &u8) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = dppd(cpu.xmm[*dst as usize], cpu.xmm[*b as usize], *imm);
    None
}

/// SSE4.1 `dppd xmm, m128, imm8` (task-256): source 2 is loaded from memory; a fault traps.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_dppd_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    imm: &u8,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let bv = match vload(mem, av, 16) {
        Ok(v) => v,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    };
    cpu.xmm[*dst as usize] = dppd(cpu.xmm[*dst as usize], bv, *imm);
    None
}

/// AVX `vdpps`/`vdppd` (task-256): the VEX 3-operand dot product with a distinct merge base
/// `a` (op1). Read `a` and `b` before writing `dst` so either aliasing `dst` is safe;
/// `prec` picks the f32/f64 helper. VEX.128 upper-zeroing is a trailing `VZeroUpper`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_dp3(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
    prec: &FPrec,
) -> Option<StepResult> {
    let (av, bv) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
    cpu.xmm[*dst as usize] = match prec {
        FPrec::F32 => dpps(av, bv, *imm),
        FPrec::F64 => dppd(av, bv, *imm),
    };
    None
}

/// As [`exec_v_dp3`] but src2 is a 128-bit memory operand (task-256). `a` is read before
/// `dst` is written so `a` aliasing `dst` is safe; a fault on the load traps.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_dp3_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm: &u8,
    prec: &FPrec,
) -> Option<StepResult> {
    let av = cpu.xmm[*a as usize]; // read merge base before dst is written
    let addr_v = read_val(*addr, &*temps);
    let bv = match vload(mem, addr_v, 16) {
        Ok(v) => v,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, addr_v, 16, AccessKind::Read, 0)),
    };
    cpu.xmm[*dst as usize] = match prec {
        FPrec::F32 => dpps(av, bv, *imm),
        FPrec::F64 => dppd(av, bv, *imm),
    };
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_align(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    shift: &u8,
    elem: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    cpu.set_vec(
        *dst as usize,
        valign_lanes(
            cpu.vec_lanes(*a as usize),
            cpu.vec_lanes(*b as usize),
            *shift,
            *elem,
            *bytes,
        ),
        *bytes,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_ternlog(
    cpu: &mut CpuState,
    dst: &u8,
    b: &u8,
    c: &u8,
    imm: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let al = cpu.vec_lanes(*dst as usize); // dst is also the first source
    let (bl, cl) = (cpu.vec_lanes(*b as usize), cpu.vec_lanes(*c as usize));
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = ternlog(al[i], bl[i], cl[i], *imm);
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_ternlog_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    b: &u8,
    addr: &Val,
    imm: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let al = cpu.vec_lanes(*dst as usize); // dst is also the first source
    let bl = cpu.vec_lanes(*b as usize);
    let base = read_val(*addr, &*temps);
    let cl = match vload_lanes(mem, base, *bytes) {
        Ok(v) => v,
        Err((ea, t)) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    };
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = ternlog(al[i], bl[i], cl[i], *imm);
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_logic256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    op: &VLogicOp,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    match vload(mem, av, 16) {
        Ok(m) => cpu.xmm[*dst as usize] = vlogic(alo, m, *op),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    }
    let hi = av.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(m) => cpu.ymm_hi[*dst as usize] = vlogic(ahi, m, *op),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_bin256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    lane: &u8,
    op: &PackedBinOp,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = packed_bin(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *lane, *op);
    cpu.ymm_hi[*dst as usize] =
        packed_bin(cpu.ymm_hi[*a as usize], cpu.ymm_hi[*b as usize], *lane, *op);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_bin256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    lane: &u8,
    op: &PackedBinOp,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    match vload(mem, av, 16) {
        Ok(m) => cpu.xmm[*dst as usize] = packed_bin(alo, m, *lane, *op),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    }
    let hi = av.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(m) => cpu.ymm_hi[*dst as usize] = packed_bin(ahi, m, *lane, *op),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

/// VEX `vpmaddwd`/`vpmaddubsw` memory form (task-260): `dst = madd(a, [addr])`, each
/// 128-bit lane via [`pmadd_lane`]. `bytes` is 16 (VEX.128 — the trailing `VZeroUpper`
/// clears bits 255:128) or 32 (VEX.256). `a` is read before `dst` is written, so `dst == a`
/// aliasing is safe.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pmadd_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    ubsw: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    match vload(mem, av, 16) {
        Ok(m) => cpu.xmm[*dst as usize] = pmadd_lane(alo, m, *ubsw),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    }
    if *bytes == 32 {
        let hi = av.wrapping_add(16);
        match vload(mem, hi, 16) {
            Ok(m) => cpu.ymm_hi[*dst as usize] = pmadd_lane(ahi, m, *ubsw),
            Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_wide(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    lane: &u8,
    op: &PackedBinOp,
    bytes: &u16,
) -> Option<StepResult> {
    let (al, bl) = (cpu.vec_lanes(*a as usize), cpu.vec_lanes(*b as usize));
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = packed_bin(al[i], bl[i], *lane, *op);
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_wide_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    lane: &u8,
    op: &PackedBinOp,
    bytes: &u16,
) -> Option<StepResult> {
    let al = cpu.vec_lanes(*a as usize);
    let base = read_val(*addr, &*temps);
    let bl = match vload_lanes(mem, base, *bytes) {
        Ok(v) => v,
        Err((ea, t)) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    };
    let mut r = [0u128; 4];
    for i in 0..4 {
        r[i] = packed_bin(al[i], bl[i], *lane, *op);
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_move_mask_b256(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    src: &u8,
) -> Option<StepResult> {
    let (lo, hi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
    temps[*dst as usize] = movemask_b(lo) | (movemask_b(hi) << 16);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_from_gpr(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &u8,
    src: &Val,
    size: &u8,
) -> Option<StepResult> {
    let v = read_val(*src, &*temps) & mask(*size);
    cpu.xmm[*dst as usize] = v as u128;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_to_gpr(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    src: &u8,
    size: &u8,
) -> Option<StepResult> {
    temps[*dst as usize] = (cpu.xmm[*src as usize] as u64) & mask(*size);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_logic(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &VLogicOp,
) -> Option<StepResult> {
    let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
    cpu.xmm[*dst as usize] = vlogic(va, vb, *op);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_bin(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    lane: &u8,
    op: &PackedBinOp,
) -> Option<StepResult> {
    let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
    cpu.xmm[*dst as usize] = packed_bin(va, vb, *lane, *op);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_bin_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    lane: &u8,
    op: &PackedBinOp,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    match vload(mem, a, 16) {
        Ok(bv) => cpu.xmm[*dst as usize] = packed_bin(cpu.xmm[*dst as usize], bv, *lane, *op),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_logic_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    op: &VLogicOp,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    match vload(mem, a, 16) {
        Ok(bv) => {
            cpu.xmm[*dst as usize] = vlogic(cpu.xmm[*dst as usize], bv, *op);
        }
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_shift(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    imm: &u8,
    lane: &u8,
    right: &bool,
    arith: &bool,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = packed_shift(cpu.xmm[*a as usize], *imm, *lane, *right, *arith);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_masked_shift(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    imm: &u8,
    elem: &u8,
    right: &bool,
    arith: &bool,
    k: &u8,
    zeroing: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    super::exec_masked_shift(
        cpu, *dst, *a, *imm, *elem, *right, *arith, *k, *zeroing, *bytes,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_byte_shift(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    shift: &u8,
    right: &bool,
    width: &u16,
) -> Option<StepResult> {
    // Per-128-bit-lane byte shift (task-262): each half shifts independently, NOT a full
    // 256-bit shift. `set_vec` zeroes above `width`.
    let av = cpu.vec_lanes(*a as usize);
    let mut r = [0u128; 4];
    for c in 0..(*width as usize / 16) {
        r[c] = byte_shift128(av[c], *shift, *right);
    }
    // Preserve the upper bits (SSE) / cleared by a trailing VZeroUpper (VEX.128); the ymm
    // form writes both halves.
    cpu.set_vec_low(*dst as usize, r, *width);
    None
}

/// `pslldq`/`psrldq` on one 128-bit lane: byte-shift by `shift` bytes (right if `right`).
fn byte_shift128(v: u128, shift: u8, right: bool) -> u128 {
    if shift >= 16 {
        0
    } else if right {
        v >> (shift as u32 * 8)
    } else {
        v << (shift as u32 * 8)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_shuffle32(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    imm: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    // In-lane dword shuffle applied to each 128-bit lane independently (task-262).
    let av = cpu.vec_lanes(*a as usize);
    let mut r = [0u128; 4];
    for c in 0..(*bytes as usize / 16) {
        r[c] = shuffle32_128(av[c], *imm);
    }
    cpu.set_vec_low(*dst as usize, r, *bytes); // SSE preserves upper; VEX.128 zeroes via VZeroUpper
    None
}

/// `pshufd`/`vpermilps`-imm on one 128-bit lane: permute the four dwords per imm8.
fn shuffle32_128(v: u128, imm: u8) -> u128 {
    let mut r = 0u128;
    for i in 0..4 {
        let sel = (imm >> (2 * i)) & 3;
        let lane = (v >> (sel as u32 * 32)) & 0xffff_ffff;
        r |= lane << (i * 32);
    }
    r
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_blend_w(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    // Per 128-bit lane the same imm8 selects each of the 8 words from `b` (bit set) or `a`
    // (task-262).
    let av = cpu.vec_lanes(*a as usize);
    let bv = cpu.vec_lanes(*b as usize);
    let mut r = [0u128; 4];
    for c in 0..(*bytes as usize / 16) {
        let mut lane = 0u128;
        for i in 0..8u32 {
            let src = if (imm >> i) & 1 != 0 { bv[c] } else { av[c] };
            lane |= ((src >> (i * 16)) & 0xffff) << (i * 16);
        }
        r[c] = lane;
    }
    cpu.set_vec_low(*dst as usize, r, *bytes); // SSE preserves upper; VEX.128 zeroes via VZeroUpper
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_blend_d(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let al = cpu.vec_lanes(*a as usize);
    let bl = cpu.vec_lanes(*b as usize);
    let mut r = [0u128; 4];
    let dwords = (*bytes / 4) as u32;
    for i in 0..dwords {
        let chunk = (i / 4) as usize;
        let sh = (i % 4) * 32;
        let src = if (imm >> i) & 1 != 0 {
            bl[chunk]
        } else {
            al[chunk]
        };
        r[chunk] |= ((src >> sh) & 0xffff_ffff) << sh;
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_fma(
    cpu: &mut CpuState,
    dst: &u8,
    x: &u8,
    y: &u8,
    z: &u8,
    prec: &FPrec,
    scalar: &bool,
    neg_prod: &bool,
    neg_add: &bool,
    bytes: &u16,
    alt_sign: &u8,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    let xv = cpu.vec_lanes(*x as usize);
    let yv = cpu.vec_lanes(*y as usize);
    let zv = cpu.vec_lanes(*z as usize);
    let old = cpu.vec_lanes(*dst as usize);
    let res = fma_lanes(
        xv, yv, zv, old, *prec, *scalar, *neg_prod, *neg_add, *bytes, *alt_sign,
    );
    // Masked EVEX packed FMA (task-201 AC#3): merge/zero the masked-off lanes at `prec`
    // element granularity. `None` (VEX / EVEX-k0) writes the full result. Scalar masked
    // forms are rejected at lift, so `scalar` implies unmasked here.
    match writemask {
        Some(k) => cpu.write_masked(*dst as usize, res, *k, prec.bytes(), *zeroing, *bytes),
        None => {
            let w = if *scalar { 16 } else { *bytes };
            cpu.set_vec(*dst as usize, res, w);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_fma_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    x: &u8,
    y: &u8,
    z: &u8,
    addr: &Val,
    mem_role: &u8,
    prec: &FPrec,
    scalar: &bool,
    neg_prod: &bool,
    neg_add: &bool,
    bytes: &u16,
    alt_sign: &u8,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    if let Some(f) = fma_mem_run(
        cpu,
        mem,
        *dst,
        *x,
        *y,
        *z,
        base,
        *mem_role,
        matches!(prec, FPrec::F64),
        *scalar,
        *neg_prod,
        *neg_add,
        *bytes,
        *alt_sign,
        cur_addr,
        *writemask,
        *zeroing,
    ) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_broadcast_lane(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    chunk: &u8,
    elem: &u8,
    dst_width: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_broadcast_lane(
        cpu,
        *dst,
        *src,
        *chunk,
        *elem,
        *dst_width,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_broadcast_lane_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    chunk: &u8,
    elem: &u8,
    dst_width: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    if let Some(f) = broadcast_lane_mem_run(
        cpu,
        mem,
        *dst,
        base,
        *chunk,
        *elem,
        *dst_width,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
        cur_addr,
    ) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pack_wide(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    from_elem: &u8,
    signed: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    exec_vpack(cpu, *dst, *a, *b, *from_elem, *signed, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pack_wide_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    from_elem: &u8,
    signed: &bool,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    match vload(mem, av, 16) {
        Ok(bv) => pack_wide_mem(cpu, *dst, bv, *from_elem, *signed),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_shuffle32_wide(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    imm: &u8,
    bytes: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vshuffle32_wide(
        cpu,
        *dst,
        *a,
        *imm,
        *bytes,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_move_half(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    dst_high: &bool,
    src_high: &bool,
) -> Option<StepResult> {
    let s = cpu.xmm[*src as usize];
    let half = if *src_high {
        s >> 64
    } else {
        s & 0xffff_ffff_ffff_ffffu128
    };
    let d = cpu.xmm[*dst as usize];
    cpu.xmm[*dst as usize] = if *dst_high {
        (d & 0xffff_ffff_ffff_ffffu128) | (half << 64)
    } else {
        (d & !0xffff_ffff_ffff_ffffu128) | half
    };
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_load_half(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    high: &bool,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    match vload(mem, a, 8) {
        Ok(v) => {
            let d = cpu.xmm[*dst as usize];
            cpu.xmm[*dst as usize] = if *high {
                (d & 0xffff_ffff_ffff_ffffu128) | (v << 64)
            } else {
                (d & !0xffff_ffff_ffff_ffffu128) | v
            };
        }
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, 8, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_store_half(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    addr: &Val,
    src: &u8,
    high: &bool,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let s = cpu.xmm[*src as usize];
    let half = if *high {
        s >> 64
    } else {
        s & 0xffff_ffff_ffff_ffffu128
    };
    if let Err(t) = vstore(mem, a, half, 8) {
        return Some(trap_out(
            cpu,
            cur_addr,
            t,
            a,
            8,
            AccessKind::Write,
            half as u64,
        ));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_extract_w(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    src: &u8,
    index: &u8,
) -> Option<StepResult> {
    let sh = (*index as u32 & 7) * 16;
    temps[*dst as usize] = ((cpu.xmm[*src as usize] >> sh) & 0xffff) as u64;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_extract_lane(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    src: &u8,
    index: &u8,
    size: &u8,
) -> Option<StepResult> {
    let bits = *size as u32 * 8;
    let sh = (*index as u32 % (128 / bits)) * bits;
    let mask = lane_mask(*size);
    temps[*dst as usize] = ((cpu.xmm[*src as usize] >> sh) & mask) as u64;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_move_mask_b(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    src: &u8,
) -> Option<StepResult> {
    temps[*dst as usize] = movemask_b(cpu.xmm[*src as usize]);
    None
}

/// movmskps/movmskpd (task-240): the sign bit of each packed-float lane of `src` → the low
/// `16/elem` bits of `dst`. `elem` = 4 (ps: 4 lanes) or 8 (pd: 2 lanes).
pub(crate) fn exec_v_move_mask_fp(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    src: &u8,
    elem: &u8,
) -> Option<StepResult> {
    let s = cpu.xmm[*src as usize];
    let bitw = *elem as u32 * 8; // 32 (ps) or 64 (pd)
    let lanes = 16 / *elem as u32; // 4 (ps) or 2 (pd)
    let mut m = 0u64;
    for i in 0..lanes {
        let sign = ((s >> (bitw * i + (bitw - 1))) & 1) as u64;
        m |= sign << i;
    }
    temps[*dst as usize] = m;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_broadcast(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    elem: &u8,
    w256: &bool,
) -> Option<StepResult> {
    let v = broadcast_elem(cpu.xmm[*src as usize], *elem);
    cpu.xmm[*dst as usize] = v;
    cpu.ymm_hi[*dst as usize] = if *w256 { v } else { 0 };
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_broadcast_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    elem: &u8,
    w256: &bool,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    match mem.read(a, *elem) {
        Ok(e) => {
            let v = broadcast_elem(e as u128, *elem);
            cpu.xmm[*dst as usize] = v;
            cpu.ymm_hi[*dst as usize] = if *w256 { v } else { 0 };
        }
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, *elem, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_broadcast_gpr(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &u8,
    src: &Val,
    elem: &u8,
    width: &u16,
) -> Option<StepResult> {
    let v = broadcast_elem(read_val(*src, &*temps) as u128, *elem);
    cpu.set_vec(*dst as usize, [v; 4], *width);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_cmp_to_mask(
    cpu: &mut CpuState,
    k: &u8,
    a: &u8,
    b: &u8,
    elem: &u8,
    width: &u16,
    pred: &u8,
    signed: &bool,
    writemask: &Option<u8>,
) -> Option<StepResult> {
    let av = cpu.vec_lanes(*a as usize);
    let bv = cpu.vec_lanes(*b as usize);
    let mut m = vpcmp_mask(av, bv, *elem, *width, *pred, *signed);
    if let Some(wk) = writemask {
        m &= cpu.kmask[*wk as usize];
    }
    cpu.kmask[*k as usize] = m;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_cmp_to_mask_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    k: &u8,
    a: &u8,
    addr: &Val,
    elem: &u8,
    width: &u16,
    pred: &u8,
    signed: &bool,
    writemask: &Option<u8>,
) -> Option<StepResult> {
    let av = cpu.vec_lanes(*a as usize);
    let base = read_val(*addr, &*temps);
    let bv = match vload_lanes(mem, base, *width) {
        Ok(v) => v,
        Err((ea, t)) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    };
    let mut m = vpcmp_mask(av, bv, *elem, *width, *pred, *signed);
    if let Some(wk) = writemask {
        m &= cpu.kmask[*wk as usize];
    }
    cpu.kmask[*k as usize] = m;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_test_to_mask(
    cpu: &mut CpuState,
    k: &u8,
    a: &u8,
    b: &u8,
    elem: &u8,
    width: &u16,
    neg: &bool,
    writemask: &Option<u8>,
) -> Option<StepResult> {
    let av = cpu.vec_lanes(*a as usize);
    let bv = cpu.vec_lanes(*b as usize);
    let mut m = vptest_mask(av, bv, *elem, *width, *neg);
    if let Some(wk) = writemask {
        m &= cpu.kmask[*wk as usize];
    }
    cpu.kmask[*k as usize] = m;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_p_test_to_mask_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    k: &u8,
    a: &u8,
    addr: &Val,
    elem: &u8,
    width: &u16,
    neg: &bool,
    writemask: &Option<u8>,
) -> Option<StepResult> {
    let av = cpu.vec_lanes(*a as usize);
    let base = read_val(*addr, &*temps);
    let bv = match vload_lanes(mem, base, *width) {
        Ok(v) => v,
        Err((ea, t)) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    };
    let mut m = vptest_mask(av, bv, *elem, *width, *neg);
    if let Some(wk) = writemask {
        m &= cpu.kmask[*wk as usize];
    }
    cpu.kmask[*k as usize] = m;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_k_or_test(
    cpu: &mut CpuState,
    a: &u8,
    b: &u8,
    width: &u8,
) -> Option<StepResult> {
    let wmask = kwidth_mask(*width);
    let t = (cpu.kmask[*a as usize] | cpu.kmask[*b as usize]) & wmask;
    cpu.flags.zf = t == 0;
    cpu.flags.cf = t == wmask;
    cpu.flags.of = false;
    cpu.flags.sf = false;
    cpu.flags.af = false;
    cpu.flags.pf = false;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_k_from_gpr(
    cpu: &mut CpuState,
    temps: &mut [u64],
    k: &u8,
    src: &Val,
    width: &u8,
) -> Option<StepResult> {
    cpu.kmask[*k as usize] = read_val(*src, &*temps) & kwidth_mask(*width);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_k_to_gpr(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &Temp,
    k: &u8,
    width: &u8,
) -> Option<StepResult> {
    temps[*dst as usize] = cpu.kmask[*k as usize] & kwidth_mask(*width);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_k_mov_k_k(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    width: &u8,
) -> Option<StepResult> {
    cpu.kmask[*dst as usize] = cpu.kmask[*src as usize] & kwidth_mask(*width);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_k_unpack(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    half: &u8,
) -> Option<StepResult> {
    let m = kwidth_mask(*half);
    let lo = cpu.kmask[*b as usize] & m;
    let hi = cpu.kmask[*a as usize] & m;
    cpu.kmask[*dst as usize] = (hi << *half) | lo;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_k_bin_op(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &VKLogicOp,
    width: &u8,
) -> Option<StepResult> {
    let ka = cpu.kmask[*a as usize];
    let kb = cpu.kmask[*b as usize];
    let r = match op {
        VKLogicOp::Or => ka | kb,
        VKLogicOp::And => ka & kb,
        VKLogicOp::Andn => !ka & kb,
        VKLogicOp::Xor => ka ^ kb,
        VKLogicOp::Xnor => !(ka ^ kb),
    };
    cpu.kmask[*dst as usize] = r & kwidth_mask(*width);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_k_not(cpu: &mut CpuState, dst: &u8, a: &u8, width: &u8) -> Option<StepResult> {
    cpu.kmask[*dst as usize] = !cpu.kmask[*a as usize] & kwidth_mask(*width);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_k_shift(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    amount: &u8,
    width: &u8,
    left: &bool,
) -> Option<StepResult> {
    let m = kwidth_mask(*width);
    let s = cpu.kmask[*a as usize] & m;
    let r = if *left {
        s.checked_shl(*amount as u32).unwrap_or(0) & m
    } else {
        s.checked_shr(*amount as u32).unwrap_or(0)
    };
    cpu.kmask[*dst as usize] = r;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pmov_narrow(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    from: &u8,
    to: &u8,
    src_width: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vpmov_narrow(
        cpu,
        *dst,
        *src,
        *from,
        *to,
        *src_width,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pmov_narrow_mem(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    src: &u8,
    addr: &Val,
    from: &u8,
    to: &u8,
    src_width: &u16,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    if let Some(f) = narrow_store_run(cpu, mem, *src, *from, *to, *src_width, base, cur_addr) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_perm_t2(
    cpu: &mut CpuState,
    dst: &u8,
    idx: &u8,
    tbl: &u8,
    elem: &u8,
    writemask: &Option<u8>,
    zeroing: &bool,
    bytes: &u16,
    imode: &bool,
) -> Option<StepResult> {
    exec_vpermt2(
        cpu,
        *dst,
        *idx,
        *tbl,
        *elem,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
        *bytes,
        *imode,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_perm_t2_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    idx: &u8,
    addr: &Val,
    elem: &u8,
    writemask: &Option<u8>,
    zeroing: &bool,
    bytes: &u16,
    imode: &bool,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    if let Some(f) = permute2_run(
        cpu,
        mem,
        *dst,
        *idx,
        base,
        *elem,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
        *bytes,
        *imode,
        cur_addr,
    ) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_perm1(
    cpu: &mut CpuState,
    dst: &u8,
    idx: &u8,
    src: &u8,
    elem: &u8,
    bytes: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vperm1(
        cpu,
        *dst,
        *idx,
        *src,
        *elem,
        *bytes,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_perm1_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    idx: &u8,
    addr: &Val,
    elem: &u8,
    bytes: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    if let Some(f) = super::vperm1_run(
        cpu,
        mem,
        *dst,
        *idx,
        base,
        *elem,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
        *bytes,
        cur_addr,
    ) {
        return Some(StepResult::Exit(str_fault_exit(f)));
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert128(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    ins: &u8,
    hi: &bool,
) -> Option<StepResult> {
    let (slo, shi, insv) = (
        cpu.xmm[*src as usize],
        cpu.ymm_hi[*src as usize],
        cpu.xmm[*ins as usize],
    );
    if *hi {
        cpu.xmm[*dst as usize] = slo;
        cpu.ymm_hi[*dst as usize] = insv;
    } else {
        cpu.xmm[*dst as usize] = insv;
        cpu.ymm_hi[*dst as usize] = shi;
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert128_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    src: &u8,
    addr: &Val,
    hi: &bool,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let insv = match vload(mem, a, 16) {
        Ok(v) => v,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0)),
    };
    let (slo, shi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
    if *hi {
        cpu.xmm[*dst as usize] = slo;
        cpu.ymm_hi[*dst as usize] = insv;
    } else {
        cpu.xmm[*dst as usize] = insv;
        cpu.ymm_hi[*dst as usize] = shi;
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_extract128(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    hi: &bool,
) -> Option<StepResult> {
    let v = if *hi {
        cpu.ymm_hi[*src as usize]
    } else {
        cpu.xmm[*src as usize]
    };
    cpu.xmm[*dst as usize] = v;
    cpu.ymm_hi[*dst as usize] = 0; // XMM destination (VEX) zeroes the upper
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pshufb256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    idx: &u8,
) -> Option<StepResult> {
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    let (ilo, ihi) = (cpu.xmm[*idx as usize], cpu.ymm_hi[*idx as usize]);
    cpu.xmm[*dst as usize] = pshufb(alo, ilo);
    cpu.ymm_hi[*dst as usize] = pshufb(ahi, ihi);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pshufb_wide(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    idx: &u8,
    bytes: &u16,
    writemask: &Option<u8>,
    zeroing: &bool,
) -> Option<StepResult> {
    exec_vpshufb_wide(
        cpu,
        *dst,
        *a,
        *idx,
        *bytes,
        writemask.unwrap_or(0),
        writemask.is_some(),
        *zeroing,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pshufb256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    match vload(mem, av, 16) {
        Ok(ilo) => cpu.xmm[*dst as usize] = pshufb(alo, ilo),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    }
    let hi = av.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(ihi) => cpu.ymm_hi[*dst as usize] = pshufb(ahi, ihi),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_shift256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    imm: &u8,
    lane: &u8,
    right: &bool,
    arith: &bool,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = packed_shift(cpu.xmm[*a as usize], *imm, *lane, *right, *arith);
    cpu.ymm_hi[*dst as usize] = packed_shift(cpu.ymm_hi[*a as usize], *imm, *lane, *right, *arith);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_permq(cpu: &mut CpuState, dst: &u8, src: &u8, imm: &u8) -> Option<StepResult> {
    let (lo, hi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
    let q = [lo as u64, (lo >> 64) as u64, hi as u64, (hi >> 64) as u64];
    let sel = |i: u32| q[((*imm >> (2 * i)) & 3) as usize] as u128;
    cpu.xmm[*dst as usize] = sel(0) | (sel(1) << 64);
    cpu.ymm_hi[*dst as usize] = sel(2) | (sel(3) << 64);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_permd(
    cpu: &mut CpuState,
    dst: &u8,
    ctrl: &u8,
    src: &u8,
) -> Option<StepResult> {
    let (clo, chi) = (cpu.xmm[*ctrl as usize], cpu.ymm_hi[*ctrl as usize]);
    let (slo, shi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
    let dword = |v: (u128, u128), i: usize| -> u64 {
        let w = if i < 4 { v.0 } else { v.1 };
        ((w >> ((i % 4) * 32)) & 0xffff_ffff) as u64
    };
    let mut lo = 0u128;
    let mut hi = 0u128;
    for i in 0..8usize {
        let idx = (dword((clo, chi), i) & 7) as usize;
        let e = dword((slo, shi), idx) as u128;
        if i < 4 {
            lo |= e << (i * 32);
        } else {
            hi |= e << ((i - 4) * 32);
        }
    }
    cpu.xmm[*dst as usize] = lo;
    cpu.ymm_hi[*dst as usize] = hi;
    None
}

/// AVX `vpermilps`/`vpermilpd` variable (register) control (task-262): an IN-LANE permute
/// applied to each 128-bit lane independently over `bytes`. For `elem`=4 (ps) the control
/// dword's bits[1:0] pick one of the 4 dwords **in the same 128-bit lane**; for `elem`=8 (pd)
/// the control qword's bit[1] picks one of the 2 qwords in that lane. Not cross-lane.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_permil_var(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    ctrl: &u8,
    elem: &u8,
    bytes: &u16,
) -> Option<StepResult> {
    let sv = cpu.vec_lanes(*src as usize);
    let cv = cpu.vec_lanes(*ctrl as usize);
    let mut r = [0u128; 4];
    for c in 0..(*bytes as usize / 16) {
        r[c] = permil_lane(sv[c], cv[c], *elem);
    }
    cpu.set_vec(*dst as usize, r, *bytes);
    None
}

/// In-lane `vpermil` on one 128-bit lane. `elem` = 4 (ps): the control dword's low 2 bits
/// select one of the 4 dwords. `elem` = 8 (pd): the control qword's bit[1] selects one of
/// the 2 qwords.
fn permil_lane(src: u128, ctrl: u128, elem: u8) -> u128 {
    let n = 16 / elem as u32; // elements per 128-bit lane
    let ebits = elem as u32 * 8;
    let emask: u128 = if elem == 8 {
        u64::MAX as u128
    } else {
        0xffff_ffff
    };
    let selbits = if elem == 4 { 2 } else { 1 }; // dword: 2-bit sel; qword: 1-bit (bit 1)
    let mut r = 0u128;
    for i in 0..n {
        let cword = ctrl >> (i * ebits);
        // ps: sel = control[1:0]; pd: sel = control[1] (bit 1 of the qword).
        let sel = if elem == 4 {
            (cword & ((1 << selbits) - 1)) as u32
        } else {
            ((cword >> 1) & 1) as u32
        };
        let e = (src >> (sel * ebits)) & emask;
        r |= e << (i * ebits);
    }
    r
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_perm2i128(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
) -> Option<StepResult> {
    let halves = [
        cpu.xmm[*a as usize],
        cpu.ymm_hi[*a as usize],
        cpu.xmm[*b as usize],
        cpu.ymm_hi[*b as usize],
    ];
    let lane = |sel: u8| -> u128 {
        if sel & 0x08 != 0 {
            0
        } else {
            halves[(sel & 3) as usize]
        }
    };
    cpu.xmm[*dst as usize] = lane(*imm);
    cpu.ymm_hi[*dst as usize] = lane(*imm >> 4);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_palignr256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = palignr(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *imm);
    cpu.ymm_hi[*dst as usize] = palignr(cpu.ymm_hi[*a as usize], cpu.ymm_hi[*b as usize], *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_ptest(cpu: &mut CpuState, a: &u8, b: &u8, w256: &bool) -> Option<StepResult> {
    let (alo, blo) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
    let (ahi, bhi) = if *w256 {
        (cpu.ymm_hi[*a as usize], cpu.ymm_hi[*b as usize])
    } else {
        (0, 0)
    };
    cpu.flags.zf = (blo & alo) == 0 && (bhi & ahi) == 0;
    cpu.flags.cf = (blo & !alo) == 0 && (bhi & !ahi) == 0;
    cpu.flags.of = false;
    cpu.flags.sf = false;
    cpu.flags.af = false;
    cpu.flags.pf = false;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_zero_upper(cpu: &mut CpuState, reg: &u8) -> Option<StepResult> {
    cpu.ymm_hi[*reg as usize] = 0;
    cpu.zmm_hi[*reg as usize] = [0; 2]; // a 128-bit write clears bits 511:128
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_zero_upper_all(cpu: &mut CpuState, clear_low: bool) -> Option<StepResult> {
    // vzeroupper/vzeroall zero bits 511:128 of ZMM0–15 (16–31 unaffected).
    cpu.ymm_hi[..16].fill(0);
    cpu.zmm_hi[..16].fill([0; 2]);
    // vzeroall additionally zeros the low 128 bits (xmm) — vzeroupper preserves them.
    if clear_low {
        cpu.xmm[..16].fill(0);
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pshufb(cpu: &mut CpuState, dst: &u8, a: &u8, idx: &u8) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = pshufb(cpu.xmm[*a as usize], cpu.xmm[*idx as usize]);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pshufb_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    match vload(mem, a, 16) {
        Ok(iv) => cpu.xmm[*dst as usize] = pshufb(cpu.xmm[*dst as usize], iv),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_alignr(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    src: &u8,
    imm: &u8,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = palignr(cpu.xmm[*a as usize], cpu.xmm[*src as usize], *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_alignr_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    imm: &u8,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    match vload(mem, a, 16) {
        Ok(iv) => cpu.xmm[*dst as usize] = palignr(cpu.xmm[*dst as usize], iv, *imm),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, 16, AccessKind::Read, 0)),
    }
    None
}

// --- AES-NI (task-205). Register + memory forms; shared pure-Rust primitives. ---

pub(crate) fn exec_v_aes(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &AesOp,
) -> Option<StepResult> {
    let state = cpu.xmm[*a as usize];
    let rk = cpu.xmm[*b as usize];
    cpu.xmm[*dst as usize] = op.apply(state, rk);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_aes_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    op: &AesOp,
) -> Option<StepResult> {
    let ea = read_val(*addr, &*temps);
    let state = cpu.xmm[*a as usize];
    match vload(mem, ea, 16) {
        Ok(rk) => cpu.xmm[*dst as usize] = op.apply(state, rk),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    }
    None
}

pub(crate) fn exec_v_aes_imc(cpu: &mut CpuState, dst: &u8, src: &u8) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = crate::aes::aes_imc(cpu.xmm[*src as usize]);
    None
}

pub(crate) fn exec_v_aes_imc_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
) -> Option<StepResult> {
    let ea = read_val(*addr, &*temps);
    match vload(mem, ea, 16) {
        Ok(v) => cpu.xmm[*dst as usize] = crate::aes::aes_imc(v),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    }
    None
}

pub(crate) fn exec_v_aes_keygen(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    imm: &u8,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = crate::aes::aes_keygen(cpu.xmm[*src as usize], *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_aes_keygen_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    imm: &u8,
) -> Option<StepResult> {
    let ea = read_val(*addr, &*temps);
    match vload(mem, ea, 16) {
        Ok(v) => cpu.xmm[*dst as usize] = crate::aes::aes_keygen(v, *imm),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    }
    None
}

// --- SHA-NI (task-207). Register + memory forms; shared pure-Rust primitives.
// `sha256rnds2` reads xmm0 implicitly (`cpu.xmm[0]`). ---

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_sha(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
    op: &ShaOp,
) -> Option<StepResult> {
    let x = cpu.xmm[*a as usize];
    let y = cpu.xmm[*b as usize];
    let xmm0 = cpu.xmm[0];
    cpu.xmm[*dst as usize] = op.apply(x, y, xmm0, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_sha_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm: &u8,
    op: &ShaOp,
) -> Option<StepResult> {
    let ea = read_val(*addr, &*temps);
    let x = cpu.xmm[*a as usize];
    let xmm0 = cpu.xmm[0];
    match vload(mem, ea, 16) {
        Ok(y) => cpu.xmm[*dst as usize] = op.apply(x, y, xmm0, *imm),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    }
    None
}

// --- GFNI (task-210). Register + memory forms; shared pure-Rust primitives. ---

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_gfni(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
    op: &GfniOp,
) -> Option<StepResult> {
    let x = cpu.xmm[*a as usize];
    let y = cpu.xmm[*b as usize];
    cpu.xmm[*dst as usize] = op.apply(x, y, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_gf2p8_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm: &u8,
    mode: &u8,
    k: &u8,
    zeroing: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    let ea = read_val(*addr, &*temps);
    super::gf2p8_mem_run(cpu, mem, *dst, *a, ea, *imm, *mode, *k, *zeroing, *bytes)
        .map(|f| StepResult::Exit(str_fault_exit(f)))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_gfni_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm: &u8,
    op: &GfniOp,
) -> Option<StepResult> {
    let ea = read_val(*addr, &*temps);
    let x = cpu.xmm[*a as usize];
    match vload(mem, ea, 16) {
        Ok(y) => cpu.xmm[*dst as usize] = op.apply(x, y, *imm),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    }
    None
}

// --- PCLMULQDQ (task-211). Register + memory forms; shared `pclmul` primitive. ---

pub(crate) fn exec_v_pclmul(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
) -> Option<StepResult> {
    let x = cpu.xmm[*a as usize];
    let y = cpu.xmm[*b as usize];
    cpu.xmm[*dst as usize] = crate::pclmul::pclmul(x, y, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pclmul_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm: &u8,
) -> Option<StepResult> {
    let ea = read_val(*addr, &*temps);
    let x = cpu.xmm[*a as usize];
    match vload(mem, ea, 16) {
        Ok(y) => cpu.xmm[*dst as usize] = crate::pclmul::pclmul(x, y, *imm),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    }
    None
}

// --- SSSE3 psign (task-210). Per `lane`-byte element (1/2/4):
// `dst[i] = ctrl[i] < 0 ? -src[i] : (ctrl[i] == 0 ? 0 : src[i])`. ---

/// Element-wise `psign` on the raw 128-bit patterns. `src` supplies the magnitude,
/// `ctrl` its sign, at `lane`-byte granularity (1=byte, 2=word, 4=dword).
pub(crate) fn psign(src: u128, ctrl: u128, lane: u8) -> u128 {
    let sb = src.to_le_bytes();
    let cb = ctrl.to_le_bytes();
    let n = lane as usize;
    let mut o = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        // Read the two little-endian lanes (up to 4 bytes) as signed values.
        let mut s: i32 = 0;
        let mut c: i32 = 0;
        for j in 0..n {
            s |= (sb[i + j] as i32) << (8 * j);
            c |= (cb[i + j] as i32) << (8 * j);
        }
        // Sign-extend from `n` bytes to i32.
        let shift = 32 - 8 * n as u32;
        let sc = (c << shift) >> shift; // signed ctrl value
        let res: i32 = if sc < 0 {
            s.wrapping_neg()
        } else if sc == 0 {
            0
        } else {
            s
        };
        let rb = res.to_le_bytes();
        o[i..i + n].copy_from_slice(&rb[0..n]);
        i += n;
    }
    u128::from_le_bytes(o)
}

pub(crate) fn exec_v_psign(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    lane: &u8,
) -> Option<StepResult> {
    let src = cpu.xmm[*a as usize];
    let ctrl = cpu.xmm[*b as usize];
    cpu.xmm[*dst as usize] = psign(src, ctrl, *lane);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_psign_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    lane: &u8,
) -> Option<StepResult> {
    let ea = read_val(*addr, &*temps);
    let src = cpu.xmm[*a as usize];
    match vload(mem, ea, 16) {
        Ok(ctrl) => cpu.xmm[*dst as usize] = psign(src, ctrl, *lane),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, ea, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
/// One 128-bit `shufps` lane: dst dwords 0,1 selected from `va`, 2,3 from `vb`, per the
/// (dword-expanded) 8-bit `imm`. Shared by the 128-bit and 256-bit (per-half) forms.
pub(crate) fn shufps128(va: u128, vb: u128, imm: u8) -> u128 {
    let mut r = 0u128;
    for i in 0..4 {
        let sel = (imm >> (2 * i)) & 3;
        let src = if i < 2 { va } else { vb };
        let lane = (src >> (sel as u32 * 32)) & 0xffff_ffff;
        r |= lane << (i as u32 * 32);
    }
    r
}

pub(crate) fn exec_v_shufps(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm: &u8,
) -> Option<StepResult> {
    let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
    cpu.xmm[*dst as usize] = shufps128(va, vb, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_shufps_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm: &u8,
) -> Option<StepResult> {
    // Read the merge base before dst is written so `a` aliasing dst is safe (VEX form).
    let va = cpu.xmm[*a as usize];
    let av = read_val(*addr, &*temps);
    let vb = match vload(mem, av, 16) {
        Ok(v) => v,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    };
    cpu.xmm[*dst as usize] = shufps128(va, vb, *imm);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_shuffle16(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    imm: &u8,
    high: &bool,
    bytes: &u16,
) -> Option<StepResult> {
    // The imm8 word-shuffle of the low (pshuflw) or high (pshufhw) 4 words is applied to
    // EACH 128-bit lane independently — NOT cross-lane (task-262).
    let av = cpu.vec_lanes(*a as usize);
    let mut r = [0u128; 4];
    for c in 0..(*bytes as usize / 16) {
        r[c] = shuffle16_128(av[c], *imm, *high);
    }
    cpu.set_vec_low(*dst as usize, r, *bytes); // SSE preserves upper; VEX.128 zeroes via VZeroUpper
    None
}

/// `pshuflw`/`pshufhw` on one 128-bit lane: shuffle the low (`high`=false) or high
/// (`high`=true) 4 words per imm8, copying the other 64-bit half unchanged.
fn shuffle16_128(v: u128, imm: u8, high: bool) -> u128 {
    let base = if high { 4u32 } else { 0 };
    let keep = if high {
        v & 0xffff_ffff_ffff_ffffu128 // preserve low 64
    } else {
        v & !0xffff_ffff_ffff_ffffu128 // preserve high 64
    };
    let mut shuf = 0u128;
    for i in 0..4 {
        let sel = (imm >> (2 * i)) & 3;
        let w = (v >> ((base + sel as u32) * 16)) & 0xffff;
        shuf |= w << ((base + i as u32) * 16);
    }
    keep | shuf
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_unpack_low(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    lane: &u8,
    high: &bool,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = unpack_low(cpu.xmm[*a as usize], cpu.xmm[*b as usize], *lane, *high);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_unpack_low_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    lane: &u8,
    high: &bool,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    match vload(mem, av, 16) {
        Ok(bv) => cpu.xmm[*dst as usize] = unpack_low(cpu.xmm[*dst as usize], bv, *lane, *high),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_pack_us_w_b(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = packuswb(cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert_w(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &u8,
    src: &Val,
    index: &u8,
) -> Option<StepResult> {
    let v = read_val(*src, &*temps) as u16 as u128;
    let sh = (*index as u32 & 7) * 16;
    let old = cpu.xmm[*dst as usize];
    cpu.xmm[*dst as usize] = (old & !(0xffffu128 << sh)) | (v << sh);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_insert_lane(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &u8,
    base: &u8,
    src: &Val,
    index: &u8,
    size: &u8,
) -> Option<StepResult> {
    let bits = *size as u32 * 8;
    let lane_mask = lane_mask(*size);
    let v = (read_val(*src, &*temps) as u128) & lane_mask;
    let sh = (*index as u32 % (128 / bits)) * bits;
    let old = cpu.xmm[*base as usize];
    cpu.xmm[*dst as usize] = (old & !(lane_mask << sh)) | (v << sh);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_mov(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    src: &u8,
    prec: &FPrec,
) -> Option<StepResult> {
    let m = lane_mask(prec.bytes());
    let s = cpu.xmm[*src as usize] & m;
    cpu.xmm[*dst as usize] = (cpu.xmm[*a as usize] & !m) | s;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_bin(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &FloatBinOp,
    prec: &FPrec,
    scalar: &bool,
) -> Option<StepResult> {
    let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
    cpu.xmm[*dst as usize] = float_bin(va, vb, *op, *prec, *scalar);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_bin_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    op: &FloatBinOp,
    prec: &FPrec,
    scalar: &bool,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    let size = if *scalar { prec.bytes() } else { 16 };
    match vload(mem, a, size) {
        Ok(bv) => {
            cpu.xmm[*dst as usize] = float_bin(cpu.xmm[*dst as usize], bv, *op, *prec, *scalar)
        }
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, size, AccessKind::Read, 0)),
    }
    None
}

// --- 256-bit VEX packed float (task-258): each 128-bit half handled independently by the
// shared per-128 helpers; VEX.256 writes the WHOLE register (both `xmm` and `ymm_hi`). ---

/// 256-bit `v{add,sub,mul,div,min,max}{ps,pd}` (register src2).
pub(crate) fn exec_v_float_bin256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &FloatBinOp,
    prec: &FPrec,
) -> Option<StepResult> {
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    let (blo, bhi) = (cpu.xmm[*b as usize], cpu.ymm_hi[*b as usize]);
    cpu.xmm[*dst as usize] = float_bin(alo, blo, *op, *prec, false);
    cpu.ymm_hi[*dst as usize] = float_bin(ahi, bhi, *op, *prec, false);
    None
}

/// 256-bit `v{add,…}{ps,pd}` with a 32-byte memory second source. The halves load
/// independently so a fault reports the exact faulting 16-byte access.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_bin256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    op: &FloatBinOp,
    prec: &FPrec,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    match vload(mem, base, 16) {
        Ok(blo) => cpu.xmm[*dst as usize] = float_bin(alo, blo, *op, *prec, false),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, base, 16, AccessKind::Read, 0)),
    }
    let hi = base.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(bhi) => cpu.ymm_hi[*dst as usize] = float_bin(ahi, bhi, *op, *prec, false),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

/// 256-bit `vsqrt{ps,pd}` (register source).
pub(crate) fn exec_v_float_unary256(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    op: &FloatUnOp,
    prec: &FPrec,
) -> Option<StepResult> {
    let (slo, shi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
    // `float_unary`'s first arg is the merge base (unused for packed), so 0 is fine.
    cpu.xmm[*dst as usize] = float_unary(0, slo, *op, *prec, false);
    cpu.ymm_hi[*dst as usize] = float_unary(0, shi, *op, *prec, false);
    None
}

/// 256-bit `vsqrt{ps,pd}` with a 32-byte memory source.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_unary256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    op: &FloatUnOp,
    prec: &FPrec,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    match vload(mem, base, 16) {
        Ok(slo) => cpu.xmm[*dst as usize] = float_unary(0, slo, *op, *prec, false),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, base, 16, AccessKind::Read, 0)),
    }
    let hi = base.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(shi) => cpu.ymm_hi[*dst as usize] = float_unary(0, shi, *op, *prec, false),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

/// 256-bit lane-preserving `vcvt{dq2ps,ps2dq,tps2dq}` (register source).
pub(crate) fn exec_v_packed_cvt256(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    kind: &PackedCvtKind,
) -> Option<StepResult> {
    let (slo, shi) = (cpu.xmm[*src as usize], cpu.ymm_hi[*src as usize]);
    cpu.xmm[*dst as usize] = packed_cvt128(slo, kind);
    cpu.ymm_hi[*dst as usize] = packed_cvt128(shi, kind);
    None
}

/// 256-bit lane-preserving `vcvt{dq2ps,ps2dq,tps2dq}` with a 32-byte memory source.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_packed_cvt256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    kind: &PackedCvtKind,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    match vload(mem, base, 16) {
        Ok(slo) => cpu.xmm[*dst as usize] = packed_cvt128(slo, kind),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, base, 16, AccessKind::Read, 0)),
    }
    let hi = base.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(shi) => cpu.ymm_hi[*dst as usize] = packed_cvt128(shi, kind),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

/// 256-bit `vshuf{ps,pd}` (register src2): per-128-lane dword shuffle, each half using its
/// own (dword-expanded) selector (`imm_lo`/`imm_hi`; equal for `vshufps`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_shufps256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    imm_lo: &u8,
    imm_hi: &u8,
) -> Option<StepResult> {
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    let (blo, bhi) = (cpu.xmm[*b as usize], cpu.ymm_hi[*b as usize]);
    cpu.xmm[*dst as usize] = shufps128(alo, blo, *imm_lo);
    cpu.ymm_hi[*dst as usize] = shufps128(ahi, bhi, *imm_hi);
    None
}

/// 256-bit `vshuf{ps,pd}` with a 32-byte memory src2.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_shufps256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    imm_lo: &u8,
    imm_hi: &u8,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    match vload(mem, base, 16) {
        Ok(blo) => cpu.xmm[*dst as usize] = shufps128(alo, blo, *imm_lo),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, base, 16, AccessKind::Read, 0)),
    }
    let hi = base.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(bhi) => cpu.ymm_hi[*dst as usize] = shufps128(ahi, bhi, *imm_hi),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

/// 256-bit `vunpck{l,h}p{s,d}` (register src2): per-128-lane float interleave over both halves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_unpack256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    lane: &u8,
    high: &bool,
) -> Option<StepResult> {
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    let (blo, bhi) = (cpu.xmm[*b as usize], cpu.ymm_hi[*b as usize]);
    cpu.xmm[*dst as usize] = unpack_low(alo, blo, *lane, *high);
    cpu.ymm_hi[*dst as usize] = unpack_low(ahi, bhi, *lane, *high);
    None
}

/// 256-bit `vunpck{l,h}p{s,d}` with a 32-byte memory src2.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_unpack256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    lane: &u8,
    high: &bool,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    match vload(mem, base, 16) {
        Ok(blo) => cpu.xmm[*dst as usize] = unpack_low(alo, blo, *lane, *high),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, base, 16, AccessKind::Read, 0)),
    }
    let hi = base.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(bhi) => cpu.ymm_hi[*dst as usize] = unpack_low(ahi, bhi, *lane, *high),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_h_float(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &HFloatOp,
    prec: &FPrec,
    bytes: &u16,
) -> Option<StepResult> {
    hfloat_reg(cpu, *dst, *a, *b, *op, *prec, *bytes);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_h_float_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    op: &HFloatOp,
    prec: &FPrec,
    bytes: &u16,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    // Load the `bytes`-wide (16/32) second source lane by 128-bit lane; a fault in either
    // lane traps on the faulting sub-address.
    let mut b = [0u128; 4];
    let lanes = *bytes as usize / 16;
    for (i, slot) in b.iter_mut().take(lanes).enumerate() {
        let la = av + (i as u64) * 16;
        match vload(mem, la, 16) {
            Ok(v) => *slot = v,
            Err(t) => return Some(trap_out(cpu, cur_addr, t, la, 16, AccessKind::Read, 0)),
        }
    }
    hfloat_mem(cpu, *dst, *a, b, *op, *prec, *bytes);
    None
}

pub(crate) fn exec_v_h_int(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    op: &HIntOp,
) -> Option<StepResult> {
    hint_reg(cpu, *dst, *a, *b, *op);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_h_int_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    op: &HIntOp,
) -> Option<StepResult> {
    let av = read_val(*addr, &*temps);
    match vload(mem, av, 16) {
        Ok(bv) => hint_mem(cpu, *dst, bv, *op),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_cmp_mask(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    prec: &FPrec,
    scalar: &bool,
    pred: &u8,
) -> Option<StepResult> {
    let (va, vb) = (cpu.xmm[*a as usize], cpu.xmm[*b as usize]);
    // Scalar merges the upper lanes from the first source `a` (= `dst` for the
    // 2-operand SSE form; a distinct src1 for the 3-operand VEX form).
    cpu.xmm[*dst as usize] = float_cmp_mask(va, va, vb, *prec, *scalar, *pred);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_cmp_mask_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    addr: &Val,
    prec: &FPrec,
    scalar: &bool,
    pred: &u8,
) -> Option<StepResult> {
    let a = read_val(*addr, &*temps);
    // Scalar compares only touch lane 0 (`prec.bytes()`); packed reads the full 16.
    let size = if *scalar { prec.bytes() } else { 16 };
    match vload(mem, a, size) {
        Ok(bv) => {
            cpu.xmm[*dst as usize] = float_cmp_mask(
                cpu.xmm[*dst as usize],
                cpu.xmm[*dst as usize],
                bv,
                *prec,
                *scalar,
                *pred,
            )
        }
        Err(t) => return Some(trap_out(cpu, cur_addr, t, a, size, AccessKind::Read, 0)),
    }
    None
}

/// 256-bit `vcmp{ps,pd}` (register src2): per-lane compare over both 128-bit halves,
/// each producing an all-ones/zero mask. VEX.256 fills bits 255:128 (the ymm_hi half).
pub(crate) fn exec_v_float_cmp_mask256(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    b: &u8,
    prec: &FPrec,
    pred: &u8,
) -> Option<StepResult> {
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    let (blo, bhi) = (cpu.xmm[*b as usize], cpu.ymm_hi[*b as usize]);
    cpu.xmm[*dst as usize] = float_cmp_mask(0, alo, blo, *prec, false, *pred);
    cpu.ymm_hi[*dst as usize] = float_cmp_mask(0, ahi, bhi, *prec, false, *pred);
    None
}

/// 256-bit `vcmp{ps,pd}` with a 32-byte memory second source. The two halves load
/// independently so a fault reports the exact faulting 16-byte access.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_cmp_mask256_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    prec: &FPrec,
    pred: &u8,
) -> Option<StepResult> {
    let base = read_val(*addr, &*temps);
    let (alo, ahi) = (cpu.xmm[*a as usize], cpu.ymm_hi[*a as usize]);
    match vload(mem, base, 16) {
        Ok(blo) => cpu.xmm[*dst as usize] = float_cmp_mask(0, alo, blo, *prec, false, *pred),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, base, 16, AccessKind::Read, 0)),
    }
    let hi = base.wrapping_add(16);
    match vload(mem, hi, 16) {
        Ok(bhi) => cpu.ymm_hi[*dst as usize] = float_cmp_mask(0, ahi, bhi, *prec, false, *pred),
        Err(t) => return Some(trap_out(cpu, cur_addr, t, hi, 16, AccessKind::Read, 0)),
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_cmp(
    cpu: &mut CpuState,
    temps: &mut [u64],
    a: &Val,
    b: &Val,
    prec: &FPrec,
) -> Option<StepResult> {
    let (zf, pf, cf) = float_compare(read_val(*a, &*temps), read_val(*b, &*temps), *prec);
    cpu.flags.zf = zf;
    cpu.flags.pf = pf;
    cpu.flags.cf = cf;
    cpu.flags.of = false;
    cpu.flags.sf = false;
    cpu.flags.af = false;
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_cvt_from_int(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &u8,
    src: &Val,
    int_size: &u8,
    prec: &FPrec,
    signed: &bool,
) -> Option<StepResult> {
    let raw = read_val(*src, &*temps);
    let bits = if *signed {
        let v = sign_extend(raw, *int_size) as i64;
        match prec {
            FPrec::F32 => (v as f32).to_bits() as u128,
            FPrec::F64 => (v as f64).to_bits() as u128,
        }
    } else {
        // Unsigned: keep only the low `int_size` bytes, cast without sign.
        let v = raw & mask(*int_size);
        match prec {
            FPrec::F32 => (v as f32).to_bits() as u128,
            FPrec::F64 => (v as f64).to_bits() as u128,
        }
    };
    let m = lane_mask(prec.bytes());
    cpu.xmm[*dst as usize] = (cpu.xmm[*dst as usize] & !m) | (bits & m);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_cvt_to_int(
    temps: &mut [u64],
    dst: &Temp,
    src: &Val,
    int_size: &u8,
    prec: &FPrec,
    trunc: &bool,
    signed: &bool,
) -> Option<StepResult> {
    let raw = read_val(*src, &*temps);
    let f = match prec {
        FPrec::F32 => f32::from_bits(raw as u32) as f64,
        FPrec::F64 => f64::from_bits(raw),
    };
    let f = if *trunc {
        f.trunc()
    } else {
        round_ties_even(f)
    };
    // Saturating cast to the destination width (Rust `as` clamps to the
    // type's MIN/MAX); matches the JIT's `fcvt_to_{s,u}int_sat`. The x86
    // integer-indefinite result on invalid operands is deferred.
    temps[*dst as usize] = match (int_size, signed) {
        (8, true) => f as i64 as u64,
        (8, false) => f as u64,
        (_, true) => f as i32 as u32 as u64,
        (_, false) => f as u32 as u64,
    };
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_cvt_float(
    cpu: &mut CpuState,
    temps: &mut [u64],
    dst: &u8,
    src: &Val,
    from: &FPrec,
    to: &FPrec,
) -> Option<StepResult> {
    let raw = read_val(*src, &*temps);
    let val = match from {
        FPrec::F32 => f32::from_bits(raw as u32) as f64,
        FPrec::F64 => f64::from_bits(raw),
    };
    let bits = match to {
        FPrec::F32 => (val as f32).to_bits() as u128,
        FPrec::F64 => val.to_bits() as u128,
    };
    let m = lane_mask(to.bytes());
    cpu.xmm[*dst as usize] = (cpu.xmm[*dst as usize] & !m) | (bits & m);
    None
}

/// Packed float↔int convert `cvt*p*` (task-239). Per-lane Rust `as` casts (saturating,
/// x86 integer-indefinite deferred — same convention as the scalar `VCvtToInt` path);
/// `round`/`trunc` mirror MXCSR-default round-to-nearest-even vs the truncating `cvtt*`.
/// The narrowing forms write the low lanes and zero the upper 64 bits, matching the JIT.
/// One 128-bit `cvt*p*` lane group (task-239/258): convert the packed lanes of `s` per
/// `kind`. Shared by the 128-bit `VPackedCvt` and the lane-preserving 256-bit `VPackedCvt256`
/// (applied to each half). The width-changing pd forms remain 128-bit-only callers.
pub(crate) fn packed_cvt128(s: u128, kind: &PackedCvtKind) -> u128 {
    let i32_lane = |i: u32| (s >> (32 * i)) as u32 as i32;
    let f32_lane = |i: u32| f32::from_bits((s >> (32 * i)) as u32);
    let f64_lane = |i: u32| f64::from_bits((s >> (64 * i)) as u64);
    let to_i = |f: f64, trunc: bool| -> u128 {
        let r = if trunc { f.trunc() } else { round_ties_even(f) };
        (r as i32 as u32) as u128
    };
    let mut o = 0u128;
    match kind {
        PackedCvtKind::Dq2Ps => {
            for i in 0..4 {
                o |= ((i32_lane(i) as f32).to_bits() as u128) << (32 * i);
            }
        }
        PackedCvtKind::Ps2Dq => {
            for i in 0..4 {
                o |= to_i(f32_lane(i) as f64, false) << (32 * i);
            }
        }
        PackedCvtKind::Tps2Dq => {
            for i in 0..4 {
                o |= to_i(f32_lane(i) as f64, true) << (32 * i);
            }
        }
        PackedCvtKind::Dq2Pd => {
            for i in 0..2 {
                o |= ((i32_lane(i) as f64).to_bits() as u128) << (64 * i);
            }
        }
        PackedCvtKind::Ps2Pd => {
            for i in 0..2 {
                o |= ((f32_lane(i) as f64).to_bits() as u128) << (64 * i);
            }
        }
        PackedCvtKind::Pd2Ps => {
            for i in 0..2 {
                o |= ((f64_lane(i) as f32).to_bits() as u128) << (32 * i);
            }
        }
        PackedCvtKind::Pd2Dq => {
            for i in 0..2 {
                o |= to_i(f64_lane(i), false) << (32 * i);
            }
        }
        PackedCvtKind::Tpd2Dq => {
            for i in 0..2 {
                o |= to_i(f64_lane(i), true) << (32 * i);
            }
        }
    }
    o
}

pub(crate) fn exec_v_packed_cvt(
    cpu: &mut CpuState,
    dst: &u8,
    src: &u8,
    kind: &PackedCvtKind,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = packed_cvt128(cpu.xmm[*src as usize], kind);
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_unary(
    cpu: &mut CpuState,
    dst: &u8,
    a: &u8,
    src: &u8,
    op: &FloatUnOp,
    prec: &FPrec,
    scalar: &bool,
) -> Option<StepResult> {
    cpu.xmm[*dst as usize] = float_unary(
        cpu.xmm[*a as usize],
        cpu.xmm[*src as usize],
        *op,
        *prec,
        *scalar,
    );
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_v_float_unary_m(
    cpu: &mut CpuState,
    mem: &Memory,
    temps: &mut [u64],
    cur_addr: u64,
    dst: &u8,
    a: &u8,
    addr: &Val,
    op: &FloatUnOp,
    prec: &FPrec,
    scalar: &bool,
) -> Option<StepResult> {
    // Read the merge base before dst is written so `a` aliasing dst is safe (VEX form).
    let base = cpu.xmm[*a as usize];
    let av = read_val(*addr, &*temps);
    // Scalar loads only the low element (prec bytes), packed loads the whole 16 bytes.
    let size = if *scalar { prec.bytes() } else { 16 };
    let src = match vload(mem, av, size) {
        Ok(v) => v,
        Err(t) => return Some(trap_out(cpu, cur_addr, t, av, size, AccessKind::Read, 0)),
    };
    // `float_unary` applies the op to lane 0 (scalar, keeping `base`'s upper) or to every
    // lane (packed, `base` unused). The loaded scalar sits in the low element of `src`.
    cpu.xmm[*dst as usize] = float_unary(base, src, *op, *prec, *scalar);
    None
}
