//! Translate an `IrBlock` to Cranelift IR (§8.2.3). One `match` on `IrOp`, but
//! describing operations to a `FunctionBuilder` instead of executing them. Flag
//! computation mirrors the interpreter (`interp.rs`) exactly so the two backends
//! agree bit-for-bit (the M4 acceptance oracle).

mod control;
mod integer;
mod memory;
mod vector;

use std::collections::HashMap;

use cranelift::prelude::*;

use cranelift::codegen::ir::{self, ConstantData, StackSlotData, StackSlotKind};

use x86jit_core::jit_abi::{
    CpuOffsets, MEMCTX_BASE, MEMCTX_EXCEPTION_VECTOR, MEMCTX_FAULT_ACCESS, MEMCTX_FAULT_ADDR,
    MEMCTX_FAULT_SIZE, MEMCTX_FUEL, MEMCTX_LINK_SLOT, MEMCTX_MEM_SELF, MEMCTX_NEXT_ENTRY,
    MEMCTX_RET_STACK, MEMCTX_SIZE, MEMCTX_WATCH_COUNT_PTR, RETSTACK_ENTRIES, RETSTACK_SP,
    RETSTACK_STRIDE, RET_CHAIN, RET_CONTINUE, RET_EXCEPTION, RET_HLT, RET_IBTC_MISS, RET_LINK,
    RET_MMIO_DEFER, RET_PORTIO_DEFER, RET_STACK_LEN, RET_SYSCALL, RET_UNMAPPED,
};
use x86jit_core::{
    AesOp, BitScanOp, BtOp, Cond, FPrec, FlagMask, FloatBinOp, FloatUnOp, GfniOp, HFloatOp, HIntOp,
    IrBlock, IrOp, IrRegion, MemConsistency, PackedBinOp, PackedCvtKind, Reg, RepKind, RmwOp,
    ShaOp, StrOp, VKLogicOp, VLogicOp, Val, VpUnaryOp,
};

const RCX: usize = 1;
const RSP: usize = 4;
const R11: usize = 11;

/// `alloc_slot` hands out a stable heap address for a link slot (a `*const u8`
/// initialized to null); the block bakes it as a constant and the dispatcher
/// fills it when the edge is first taken (§12 M5). `div_ref` is the imported
/// division helper.
/// Imported Rust helpers callable from compiled blocks (§14, §10).
/// Each helper is `(signature, absolute fn address)`: compiled blocks reach them
/// via `call_indirect` through a baked address rather than a linker-relocated
/// direct call, so the emitted machine code carries **no relocations** (the
/// prerequisite for a persistable AOT code cache — see backlog/docs/design/aot-plan.md).
#[derive(Copy, Clone)]
pub struct Helpers {
    pub div: (ir::SigRef, u64),
    pub string: (ir::SigRef, u64),
    pub cpuid: (ir::SigRef, u64),
    pub xgetbv: (ir::SigRef, u64),
    pub vmaskmov: (ir::SigRef, u64),
    pub vmaskmov_mem: (ir::SigRef, u64),
    pub vmasked_logic: (ir::SigRef, u64),
    pub valign: (ir::SigRef, u64),
    pub vpermt2: (ir::SigRef, u64),
    pub vpermt2_mem: (ir::SigRef, u64),
    pub vperm1: (ir::SigRef, u64),
    pub vperm1_mem: (ir::SigRef, u64),
    pub vpmov_narrow: (ir::SigRef, u64),
    pub vpmov_narrow_mem: (ir::SigRef, u64),
    pub vpmov_extend_wide: (ir::SigRef, u64),
    pub vpabs: (ir::SigRef, u64),
    pub vp_unary_lane: (ir::SigRef, u64),
    pub vp_blendm: (ir::SigRef, u64),
    pub vshuf_lane: (ir::SigRef, u64),
    pub vp_multishift: (ir::SigRef, u64),
    pub vpshufb_wide: (ir::SigRef, u64),
    pub vshuffle32_wide: (ir::SigRef, u64),
    pub vpack: (ir::SigRef, u64),
    pub vpack_mem: (ir::SigRef, u64),
    pub vhfloat: (ir::SigRef, u64),
    pub vhfloat_mem: (ir::SigRef, u64),
    pub vhint: (ir::SigRef, u64),
    pub vhint_mem: (ir::SigRef, u64),
    pub pmaddwd: (ir::SigRef, u64),
    pub fma: (ir::SigRef, u64),
    pub fma_mem: (ir::SigRef, u64),
    pub broadcast_lane: (ir::SigRef, u64),
    pub broadcast_lane_mem: (ir::SigRef, u64),
    pub aes: (ir::SigRef, u64),
    pub aes_mem: (ir::SigRef, u64),
    pub sha: (ir::SigRef, u64),
    pub sha_mem: (ir::SigRef, u64),
    pub gfni: (ir::SigRef, u64),
    pub gfni_mem: (ir::SigRef, u64),
    pub pclmul: (ir::SigRef, u64),
    pub pclmul_mem: (ir::SigRef, u64),
    pub mmx_bridge: (ir::SigRef, u64),
    pub vmasked_packed: (ir::SigRef, u64),
    pub vmasked_shift: (ir::SigRef, u64),
    pub var_shift: (ir::SigRef, u64),
    pub shift_reg: (ir::SigRef, u64),
    pub gf2p8: (ir::SigRef, u64),
    pub gf2p8_mem: (ir::SigRef, u64),
    pub pcmpstr_mem: (ir::SigRef, u64),
    pub pcmpstr: (ir::SigRef, u64),
    pub pcmpstrm: (ir::SigRef, u64),
    pub pcmpstrm_mem: (ir::SigRef, u64),
    pub bmi: (ir::SigRef, u64),
    pub x87: (ir::SigRef, u64),
    pub fxstate: (ir::SigRef, u64),
    pub crc32: (ir::SigRef, u64),
    pub note_watch: (ir::SigRef, u64),
}

#[allow(clippy::too_many_arguments)]
pub fn translate_block(
    builder: &mut FunctionBuilder,
    ir: &IrBlock,
    offsets: &CpuOffsets,
    alloc_slot: &mut dyn FnMut() -> u64,
    helpers: Helpers,
    consistency: MemConsistency,
    mmio: Option<(u64, u64)>,
    guest_base: u64,
) {
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let cpu = builder.block_params(entry)[0];
    let mem = builder.block_params(entry)[1];

    let mut t = Translator {
        builder,
        offsets,
        cpu,
        mem,
        temps: vec![None; ir.temp_count as usize],
        cur_addr: ir.guest_start,
        guest_end: ir.guest_start + ir.guest_len as u64,
        alloc_slot,
        helpers,
        consistency,
        gpr_cache: [None; 16],
        gpr_vars: None,
        fuel_var: None,
        mmio,
        guest_base,
        checked_ea: Vec::new(),
    };

    let mut terminated = false;
    for op in &ir.ops {
        if t.op(op) {
            terminated = true;
            break;
        }
    }
    if !terminated {
        // Straight-line block with no control-flow terminator: flow on past it.
        let end = t.iconst(t.guest_end);
        t.store_cpu(offsets.rip, end);
        t.ret(RET_CONTINUE);
    }
}

/// Translate a **superblock region** (§12 M5-T3) into one function: its sub-blocks
/// each become one Cranelift block, wired by the guest control flow. Static
/// forward/merge edges (an unconditional jump, or a conditional branch arm, to a
/// block later in reverse-post-order) become internal Cranelift `jump`/`brif`;
/// back-edges (loops) and edges leaving the region become chain/link exits — so a
/// loop still returns to the dispatcher each iteration (proper loop bodies arrive
/// in T3d). Each block starts with a fuel gate that charges one guest block and
/// exits when the budget is spent, keeping the block count exact for §9.2 / the
/// `Blocks(n)` oracle. Registers/flags stay write-through, so `CpuState` is current
/// at every gate exit and every trap — no register flush yet (that is T3e).
#[allow(clippy::too_many_arguments)]
pub fn translate_region(
    builder: &mut FunctionBuilder,
    region: &IrRegion,
    offsets: &CpuOffsets,
    alloc_slot: &mut dyn FnMut() -> u64,
    helpers: Helpers,
    consistency: MemConsistency,
    mmio: Option<(u64, u64)>,
    guest_base: u64,
) {
    let fentry = builder.create_block();
    builder.append_block_params_for_function_params(fentry);
    builder.switch_to_block(fentry);
    let cpu = builder.block_params(fentry)[0];
    let mem = builder.block_params(fentry)[1];

    // One Cranelift block per sub-block, keyed by guest address. Any edge whose
    // target is in this map is internal (incl. back-edges → loops, T3d).
    let mut clif: HashMap<u64, Block> = HashMap::new();
    for b in &region.blocks {
        clif.insert(b.guest_start, builder.create_block());
    }

    let mut t = Translator {
        builder,
        offsets,
        cpu,
        mem,
        temps: Vec::new(),
        cur_addr: region.entry,
        guest_end: region.entry,
        alloc_slot,
        helpers,
        consistency,
        gpr_cache: [None; 16],
        gpr_vars: None,
        fuel_var: None,
        mmio,
        guest_base,
        checked_ea: Vec::new(),
    };

    // Carry the 16 GPRs as SSA Variables (§12 M5-T3e): declare them, seed each from
    // `CpuState` in the entry block (which dominates everything), and switch to
    // Variable mode. `ret` flushes them back at every exit.
    let mut gpr_vars = [Variable::new(0); 16];
    for (i, slot) in gpr_vars.iter_mut().enumerate() {
        let var = Variable::new(i);
        t.builder.declare_var(var, types::I64);
        *slot = var;
    }
    for (i, &var) in gpr_vars.iter().enumerate() {
        let v = t.load_cpu(t.offsets.gpr(i));
        t.builder.def_var(var, v);
    }
    t.gpr_vars = Some(gpr_vars);

    // Fuel is likewise a carried Variable (seeded from MemCtx), so the per-block
    // gate is a register decrement, not a load+store.
    let fuel_var = Variable::new(16);
    t.builder.declare_var(fuel_var, types::I64);
    let init_fuel = t.load_mem(MEMCTX_FUEL);
    t.builder.def_var(fuel_var, init_fuel);
    t.fuel_var = Some(fuel_var);

    // The entry block flows into the first sub-block.
    let first = clif[&region.entry];
    t.builder.ins().jump(first, &[]);

    for block in &region.blocks {
        t.builder.switch_to_block(clif[&block.guest_start]);
        t.emit_fuel_gate(block.guest_start); // charge on entry; exit if the budget is spent
        t.temps = vec![None; block.temp_count as usize];
        t.gpr_cache = [None; 16];
        t.checked_ea.clear(); // a checked pointer only dominates its own block
        t.cur_addr = block.guest_start;
        t.guest_end = block.guest_start + block.guest_len as u64;
        t.emit_region_block(block, &clif);
    }

    t.builder.seal_all_blocks();
}

struct Translator<'a, 'b> {
    builder: &'a mut FunctionBuilder<'b>,
    offsets: &'a CpuOffsets,
    cpu: Value,
    mem: Value,
    temps: Vec<Option<Value>>,
    cur_addr: u64,
    guest_end: u64,
    alloc_slot: &'a mut dyn FnMut() -> u64,
    helpers: Helpers,
    /// Memory-consistency tier for ordinary guest loads/stores (§8.2.3). Only
    /// affects an ARM host; on x86 every tier emits identical (plain) code.
    consistency: MemConsistency,
    /// Block-local cache of each GPR's current Cranelift value (`None` = not yet
    /// loaded). Writes are write-through (they still store to `CpuState`, so guest
    /// state is current at every trap/exit — no flush needed) *and* update the
    /// cache, so a reload of a just-written or just-read register reuses the SSA
    /// value instead of round-tripping through memory Cranelift can't prove is
    /// non-aliasing with guest RAM. Invalidated after any helper that mutates GPRs.
    /// Used only in single-block mode (`gpr_vars` is `None`).
    gpr_cache: [Option<Value>; 16],
    /// Region mode (§12 M5-T3e): the 16 GPRs are carried as Cranelift `Variable`s,
    /// so reads/writes stay in host registers across the whole region (loop bodies
    /// especially) instead of round-tripping through `CpuState`. Loaded once at
    /// region entry and **flushed** to `CpuState` at every exit/trap (`ret`), so the
    /// dispatcher and helpers always see current state. `None` in single-block mode,
    /// where writes are write-through instead.
    gpr_vars: Option<[Variable; 16]>,
    /// Region mode: `MemCtx.fuel` carried as a Variable so the per-block gate is a
    /// register decrement, not a load+store. Loaded at entry, flushed at every exit
    /// (in `ret`, next to the GPRs). `None` in single-block mode.
    fuel_var: Option<Variable>,
    /// The guest's `Trap`-region window `[lo, hi)` baked as a constant (§5.2,
    /// M4-T10), or `None` when the VM has no MMIO regions. When `Some`, every
    /// inlined load/store gets a range check that defers a Trap-region access to the
    /// interpreter (`RET_MMIO_DEFER`); `None` emits no check — the common, zero-cost
    /// case.
    mmio: Option<(u64, u64)>,
    /// Guest base (host addr of the RAM buffer's first byte, §4.1) baked as a
    /// compile-time constant. `0` — the common zero-based layout — emits the historical
    /// `host = base + guest_addr` with no rebase and no lower-bound check, so codegen is
    /// byte-identical. A non-zero base (identity mapping) rebases every inlined access
    /// to `host = base + (guest_addr - guest_base)` and rejects a below-base address.
    guest_base: u64,
    /// Bounds checks already emitted in the current basic block: `(addr, size) →
    /// host pointer` (task-155). A read-modify-write instruction (`add [mem], rax`)
    /// lifts to `Load`+`Store` on the *same* effective-address value; the second
    /// access reuses the first's checked host pointer instead of re-emitting the
    /// bound check + branch. Cleared at every basic-block boundary — the cached
    /// pointer only dominates uses in its own straight-line block.
    checked_ea: Vec<(Value, u8, Value)>,
}

impl Translator<'_, '_> {
    /// Translate one op; return `true` if it terminated the block.
    fn op(&mut self, op: &IrOp) -> bool {
        match op {
            IrOp::InsnStart { guest_addr, .. } => self.emit_insn_start(guest_addr),
            IrOp::ReadReg { dst, reg, .. } => self.emit_read_reg(dst, reg),
            IrOp::WriteReg { reg, src, size, .. } => self.emit_write_reg(reg, src, size),
            IrOp::Add {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_add(dst, a, b, size, set_flags),
            IrOp::Adc {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_adc(dst, a, b, size, set_flags),
            IrOp::Sub {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_sub(dst, a, b, size, set_flags),
            IrOp::Sbb {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_sbb(dst, a, b, size, set_flags),
            IrOp::And {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_and(dst, a, b, size, set_flags),
            IrOp::Or {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_or(dst, a, b, size, set_flags),
            IrOp::Xor {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_xor(dst, a, b, size, set_flags),
            IrOp::Shl {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_shl(dst, a, b, size, set_flags),
            IrOp::Shr {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_shr(dst, a, b, size, set_flags),
            IrOp::Sar {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_sar(dst, a, b, size, set_flags),
            IrOp::Rol {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_rol(dst, a, b, size, set_flags),
            IrOp::Ror {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_ror(dst, a, b, size, set_flags),
            IrOp::Rcl {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_rcl(dst, a, b, size, set_flags),
            IrOp::Rcr {
                dst,
                a,
                b,
                size,
                set_flags,
                ..
            } => self.emit_rcr(dst, a, b, size, set_flags),
            IrOp::DoubleShift {
                dst,
                a,
                b,
                count,
                size,
                left,
                set_flags,
                ..
            } => self.emit_double_shift_arm(dst, a, b, count, size, left, set_flags),
            IrOp::Sext { dst, a, from, .. } => self.emit_sext(dst, a, from),
            IrOp::Bswap { dst, a, size, .. } => self.emit_bswap(dst, a, size),
            IrOp::Mul {
                lo,
                hi,
                a,
                b,
                size,
                signed,
                set_flags,
                ..
            } => self.emit_mul_arm(lo, hi, a, b, size, signed, set_flags),
            IrOp::Div {
                quot,
                rem,
                hi,
                lo,
                divisor,
                size,
                signed,
                ..
            } => self.emit_div_arm(quot, rem, hi, lo, divisor, size, signed),
            IrOp::GetCond { dst, cond, .. } => self.emit_get_cond(dst, cond),
            IrOp::Load {
                dst, addr, size, ..
            } => self.emit_load(dst, addr, size),
            IrOp::Store {
                addr, src, size, ..
            } => self.emit_store(addr, src, size),
            IrOp::AtomicRmw {
                old,
                addr,
                src,
                size,
                op,
                ..
            } => self.emit_atomic_rmw(old, addr, src, size, op),
            IrOp::AtomicCas {
                old,
                addr,
                expected,
                src,
                size,
                ..
            } => self.emit_atomic_cas(old, addr, expected, src, size),
            IrOp::Bt {
                result,
                a,
                bit,
                size,
                op,
                ..
            } => self.emit_bt(result, a, bit, size, op),
            IrOp::Cpuid => self.emit_cpuid(),
            IrOp::Xgetbv => self.emit_xgetbv(),
            IrOp::X87 {
                kind, addr, sti, ..
            } => self.emit_x87(kind, addr, sti),
            IrOp::FxState { addr, restore, .. } => self.emit_fx_state(addr, restore),
            IrOp::Popcnt { dst, src, size, .. } => self.emit_popcnt(dst, src, size),
            IrOp::Crc32 {
                dst,
                crc,
                src,
                bytes,
                ..
            } => self.emit_crc32(dst, crc, src, bytes),
            IrOp::BitScan {
                dst,
                src,
                old,
                size,
                op,
                ..
            } => self.emit_bit_scan(dst, src, old, size, op),
            IrOp::Bmi {
                dst,
                a,
                b,
                size,
                op,
                ..
            } => self.emit_bmi(dst, a, b, size, op),
            IrOp::VLoad {
                dst, addr, size, ..
            } => self.emit_v_load(dst, addr, size),
            IrOp::VStore {
                addr, src, size, ..
            } => self.emit_v_store(addr, src, size),
            IrOp::VMov { dst, src, .. } => self.emit_v_mov(dst, src),
            IrOp::VLoadWide {
                dst, addr, bytes, ..
            } => self.emit_v_load_wide(dst, addr, bytes),
            IrOp::VStoreWide {
                addr, src, bytes, ..
            } => self.emit_v_store_wide(addr, src, bytes),
            IrOp::VMovWide {
                dst, src, bytes, ..
            } => self.emit_v_mov_wide(dst, src, bytes),
            IrOp::VMaskMov {
                dst,
                src,
                k,
                elem,
                zeroing,
                bytes,
                ..
            } => self.emit_v_mask_mov(dst, src, k, elem, zeroing, bytes),
            IrOp::VMaskLoadMem {
                dst,
                addr,
                k,
                elem,
                zeroing,
                bytes,
                ..
            } => self.emit_v_mask_load_mem(dst, addr, k, elem, zeroing, bytes),
            IrOp::VMaskStoreMem {
                src,
                addr,
                k,
                elem,
                bytes,
                ..
            } => self.emit_v_mask_store_mem(src, addr, k, elem, bytes),
            IrOp::VInsertLaneWide {
                dst,
                src,
                ins,
                idx,
                num_lanes,
                bytes,
                ..
            } => self.emit_v_insert_lane_wide(dst, src, ins, idx, num_lanes, bytes),
            IrOp::VExtractLaneWide {
                dst,
                src,
                idx,
                num_lanes,
                ..
            } => self.emit_v_extract_lane_wide(dst, src, idx, num_lanes),
            IrOp::VExtractLaneWideM {
                src,
                addr,
                idx,
                num_lanes,
                ..
            } => self.emit_v_extract_lane_wide_m(src, addr, idx, num_lanes),
            IrOp::VPcmpStr {
                a,
                b,
                imm,
                explicit,
                ..
            } => self.emit_v_pcmp_str(a, b, imm, explicit),
            IrOp::VPcmpStrM {
                a,
                addr,
                imm,
                explicit,
                ..
            } => self.emit_v_pcmp_str_m(a, addr, imm, explicit),
            IrOp::VPcmpStrMask {
                a,
                b,
                imm,
                explicit,
                ..
            } => self.emit_v_pcmp_str_mask(a, b, imm, explicit),
            IrOp::VPcmpStrMaskM {
                a,
                addr,
                imm,
                explicit,
                ..
            } => self.emit_v_pcmp_str_mask_m(a, addr, imm, explicit),
            IrOp::VInsertPs { dst, src, imm, .. } => self.emit_v_insert_ps(dst, src, imm),
            IrOp::VInsertPsM { dst, addr, imm, .. } => self.emit_v_insert_ps_m(dst, addr, imm),
            IrOp::VDpps { dst, b, imm, .. } => self.emit_v_dpps(dst, b, imm),
            IrOp::VDppsM { dst, addr, imm, .. } => self.emit_v_dpps_m(dst, addr, imm),
            IrOp::VAlign {
                dst,
                a,
                b,
                shift,
                elem,
                bytes,
                ..
            } => self.emit_v_align(dst, a, b, shift, elem, bytes),
            IrOp::VPermT2 {
                dst,
                idx,
                tbl,
                elem,
                writemask,
                zeroing,
                bytes,
                imode,
                ..
            } => self.emit_v_perm_t2(dst, idx, tbl, elem, writemask, zeroing, bytes, imode),
            IrOp::VPermT2M {
                dst,
                idx,
                addr,
                elem,
                writemask,
                zeroing,
                bytes,
                imode,
                ..
            } => self.emit_v_perm_t2_m(dst, idx, addr, elem, writemask, zeroing, bytes, imode),
            IrOp::VPerm1 {
                dst,
                idx,
                src,
                elem,
                bytes,
                writemask,
                zeroing,
                ..
            } => self.emit_v_perm1(dst, idx, src, elem, bytes, writemask, zeroing),
            IrOp::VPerm1M {
                dst,
                idx,
                addr,
                elem,
                bytes,
                writemask,
                zeroing,
            } => self.emit_v_perm1_m(dst, idx, addr, elem, bytes, writemask, zeroing),
            IrOp::VMaskedLogic {
                dst,
                a,
                b,
                op,
                k,
                elem,
                zeroing,
                bytes,
                ..
            } => self.emit_v_masked_logic(dst, a, b, op, k, elem, zeroing, bytes),
            IrOp::VMaskedPacked {
                dst,
                a,
                b,
                op,
                k,
                elem,
                zeroing,
                bytes,
                ..
            } => self.emit_v_masked_packed(dst, a, b, op, k, elem, zeroing, bytes),
            IrOp::VMaskedShift {
                dst,
                a,
                imm,
                elem,
                right,
                arith,
                k,
                zeroing,
                bytes,
            } => self.emit_v_masked_shift(dst, a, imm, elem, right, arith, k, zeroing, bytes),
            IrOp::VShiftVar {
                dst,
                a,
                count,
                elem,
                right,
                arith,
                k,
                zeroing,
                bytes,
            } => self.emit_v_shift_var(dst, a, count, elem, right, arith, k, zeroing, bytes),
            IrOp::VShiftReg {
                dst,
                a,
                count,
                elem,
                right,
                arith,
                k,
                zeroing,
                bytes,
            } => self.emit_v_shift_reg(dst, a, count, elem, right, arith, k, zeroing, bytes),
            IrOp::VGf2p8 {
                dst,
                a,
                b,
                imm,
                mode,
                k,
                zeroing,
                bytes,
            } => self.emit_v_gf2p8(dst, a, b, imm, mode, k, zeroing, bytes),
            IrOp::VGf2p8M {
                dst,
                a,
                addr,
                imm,
                mode,
                k,
                zeroing,
                bytes,
            } => self.emit_v_gf2p8_m(dst, a, addr, imm, mode, k, zeroing, bytes),
            IrOp::VLogic256 { dst, a, b, op, .. } => self.emit_v_logic256(dst, a, b, op),
            IrOp::VLogicWide {
                dst,
                a,
                b,
                op,
                bytes,
                ..
            } => self.emit_v_logic_wide(dst, a, b, op, bytes),
            IrOp::VLogicWideM {
                dst,
                a,
                addr,
                op,
                bytes,
                ..
            } => self.emit_v_logic_wide_m(dst, a, addr, op, bytes),
            IrOp::VPopcnt {
                dst,
                a,
                lane,
                bytes,
                ..
            } => self.emit_v_popcnt(dst, a, lane, bytes),
            IrOp::VPopcntM {
                dst,
                addr,
                lane,
                bytes,
                ..
            } => self.emit_v_popcnt_m(dst, addr, lane, bytes),
            IrOp::VPMovExtend {
                dst,
                src,
                from,
                to,
                signed,
                ..
            } => self.emit_v_p_mov_extend(dst, src, from, to, signed),
            IrOp::VPMovExtendM {
                dst,
                addr,
                from,
                to,
                signed,
                ..
            } => self.emit_v_p_mov_extend_m(dst, addr, from, to, signed),
            IrOp::VPMovExtendWide {
                dst,
                src,
                from,
                to,
                signed,
                dst_width,
                writemask,
                zeroing,
                ..
            } => self.emit_v_p_mov_extend_wide(
                dst, src, from, to, signed, dst_width, writemask, zeroing,
            ),
            IrOp::VPAbs {
                dst,
                src,
                elem,
                dst_width,
                writemask,
                zeroing,
                ..
            } => self.emit_v_p_abs(dst, src, elem, dst_width, writemask, zeroing),
            IrOp::VpUnaryLane {
                dst,
                src,
                op,
                imm,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => self.emit_v_p_unary_lane(dst, src, op, imm, elem, dst_width, writemask, zeroing),
            IrOp::VpBlendm {
                dst,
                a,
                b,
                k,
                elem,
                dst_width,
                zeroing,
            } => self.emit_v_p_blendm(dst, a, b, k, elem, dst_width, zeroing),
            IrOp::VShuffLane {
                dst,
                a,
                b,
                imm,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => self.emit_v_shuf_lane(dst, a, b, imm, elem, dst_width, writemask, zeroing),
            IrOp::VpMultishift {
                dst,
                ctrl,
                data,
                dst_width,
                writemask,
                zeroing,
            } => self.emit_v_p_multishift(dst, ctrl, data, dst_width, writemask, zeroing),
            IrOp::VPBlendV { dst, src, lane, .. } => self.emit_v_p_blend_v(dst, src, lane),
            IrOp::VPBlendVM {
                dst, addr, lane, ..
            } => self.emit_v_p_blend_v_m(dst, addr, lane),
            IrOp::VPBlendVX {
                dst,
                a,
                b,
                mask,
                lane,
            } => self.emit_v_p_blend_v_x(dst, a, b, mask, lane),
            IrOp::VPRound {
                dst,
                a,
                src,
                prec,
                mode,
                scalar,
                ..
            } => self.emit_v_p_round(dst, a, src, prec, mode, scalar),
            IrOp::VPRoundM {
                dst,
                addr,
                prec,
                mode,
                scalar,
                ..
            } => self.emit_v_p_round_m(dst, addr, prec, mode, scalar),
            IrOp::VPTernlog {
                dst,
                b,
                c,
                imm,
                bytes,
                ..
            } => self.emit_v_p_ternlog(dst, b, c, imm, bytes),
            IrOp::VPTernlogM {
                dst,
                b,
                addr,
                imm,
                bytes,
                ..
            } => self.emit_v_p_ternlog_m(dst, b, addr, imm, bytes),
            IrOp::VLogic256M {
                dst, a, addr, op, ..
            } => self.emit_v_logic256_m(dst, a, addr, op),
            IrOp::VPackedBin256 {
                dst,
                a,
                b,
                lane,
                op,
                ..
            } => self.emit_v_packed_bin256(dst, a, b, lane, op),
            IrOp::VPackedBin256M {
                dst,
                a,
                addr,
                lane,
                op,
                ..
            } => self.emit_v_packed_bin256_m(dst, a, addr, lane, op),
            IrOp::VPackedWide {
                dst,
                a,
                b,
                lane,
                op,
                bytes,
                ..
            } => self.emit_v_packed_wide(dst, a, b, lane, op, bytes),
            IrOp::VPackedWideM {
                dst,
                a,
                addr,
                lane,
                op,
                bytes,
                ..
            } => self.emit_v_packed_wide_m(dst, a, addr, lane, op, bytes),
            IrOp::VMoveMaskB256 { dst, src, .. } => self.emit_v_move_mask_b256(dst, src),
            IrOp::VBroadcastGpr {
                dst,
                src,
                elem,
                width,
                ..
            } => self.emit_v_broadcast_gpr(dst, src, elem, width),
            IrOp::VBroadcastLane {
                dst,
                src,
                chunk,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => self.emit_v_broadcast_lane(dst, src, chunk, elem, dst_width, writemask, zeroing),
            IrOp::VBroadcastLaneM {
                dst,
                addr,
                chunk,
                elem,
                dst_width,
                writemask,
                zeroing,
            } => {
                self.emit_v_broadcast_lane_m(dst, addr, chunk, elem, dst_width, writemask, zeroing)
            }
            IrOp::VPCmpToMask {
                k,
                a,
                b,
                elem,
                width,
                pred,
                signed,
                writemask,
                ..
            } => self.emit_v_p_cmp_to_mask(k, a, b, elem, width, pred, signed, writemask),
            IrOp::VPCmpToMaskM {
                k,
                a,
                addr,
                elem,
                width,
                pred,
                signed,
                writemask,
                ..
            } => self.emit_v_p_cmp_to_mask_m(k, a, addr, elem, width, pred, signed, writemask),
            IrOp::VPTestToMask {
                k,
                a,
                b,
                elem,
                width,
                neg,
                writemask,
                ..
            } => self.emit_v_p_test_to_mask(k, a, b, elem, width, neg, writemask),
            IrOp::VPTestToMaskM {
                k,
                a,
                addr,
                elem,
                width,
                neg,
                writemask,
                ..
            } => self.emit_v_p_test_to_mask_m(k, a, addr, elem, width, neg, writemask),
            IrOp::VKOrTest { a, b, width, .. } => self.emit_v_k_or_test(a, b, width),
            IrOp::VKFromGpr { k, src, width, .. } => self.emit_v_k_from_gpr(k, src, width),
            IrOp::VKToGpr { dst, k, width, .. } => self.emit_v_k_to_gpr(dst, k, width),
            IrOp::VKMovKK {
                dst, src, width, ..
            } => self.emit_v_k_mov_k_k(dst, src, width),
            IrOp::VKUnpack {
                dst, a, b, half, ..
            } => self.emit_v_k_unpack(dst, a, b, half),
            IrOp::VKBinOp {
                dst,
                a,
                b,
                op,
                width,
                ..
            } => self.emit_v_k_bin_op(dst, a, b, op, width),
            IrOp::VKNot { dst, a, width, .. } => self.emit_v_k_not(dst, a, width),
            IrOp::VKShift {
                dst,
                a,
                amount,
                width,
                left,
                ..
            } => self.emit_v_k_shift(dst, a, amount, width, left),
            IrOp::VPmovNarrow {
                dst,
                src,
                from,
                to,
                src_width,
                writemask,
                zeroing,
                ..
            } => self.emit_v_pmov_narrow(dst, src, from, to, src_width, writemask, zeroing),
            IrOp::VPmovNarrowMem {
                src,
                addr,
                from,
                to,
                src_width,
                ..
            } => self.emit_v_pmov_narrow_mem(src, addr, from, to, src_width),
            IrOp::VBroadcast {
                dst,
                src,
                elem,
                w256,
                ..
            } => self.emit_v_broadcast(dst, src, elem, w256),
            IrOp::VBroadcastM {
                dst,
                addr,
                elem,
                w256,
                ..
            } => self.emit_v_broadcast_m(dst, addr, elem, w256),
            IrOp::VInsert128 {
                dst, src, ins, hi, ..
            } => self.emit_v_insert128(dst, src, ins, hi),
            IrOp::VInsert128M {
                dst, src, addr, hi, ..
            } => self.emit_v_insert128_m(dst, src, addr, hi),
            IrOp::VExtract128 { dst, src, hi, .. } => self.emit_v_extract128(dst, src, hi),
            IrOp::VFromGpr { dst, src, size, .. } => self.emit_v_from_gpr(dst, src, size),
            IrOp::VToGpr { dst, src, size, .. } => self.emit_v_to_gpr(dst, src, size),
            IrOp::VLogic { dst, a, b, op, .. } => self.emit_v_logic(dst, a, b, op),
            IrOp::VPackedBin {
                dst,
                a,
                b,
                lane,
                op,
                ..
            } => self.emit_v_packed_bin(dst, a, b, lane, op),
            IrOp::VPackedBinM {
                dst,
                addr,
                lane,
                op,
                ..
            } => self.emit_v_packed_bin_m(dst, addr, lane, op),
            IrOp::VLogicM { dst, addr, op, .. } => self.emit_v_logic_m(dst, addr, op),
            IrOp::VPackedShift {
                dst,
                a,
                imm,
                lane,
                right,
                arith,
                ..
            } => self.emit_v_packed_shift(dst, a, imm, lane, right, arith),
            IrOp::VPackedShift256 {
                dst,
                a,
                imm,
                lane,
                right,
                arith,
                ..
            } => self.emit_v_packed_shift256(dst, a, imm, lane, right, arith),
            IrOp::VPermq { dst, src, imm, .. } => self.emit_v_permq(dst, src, imm),
            IrOp::VPermd { dst, ctrl, src, .. } => self.emit_v_permd(dst, ctrl, src),
            IrOp::VPerm2i128 { dst, a, b, imm, .. } => self.emit_v_perm2i128(dst, a, b, imm),
            IrOp::VPalignr256 { dst, a, b, imm, .. } => self.emit_v_palignr256(dst, a, b, imm),
            IrOp::VPtest { a, b, w256, .. } => self.emit_v_ptest(a, b, w256),
            IrOp::VPshufb256 { dst, a, idx, .. } => self.emit_v_pshufb256(dst, a, idx),
            IrOp::VPshufbWide {
                dst,
                a,
                idx,
                bytes,
                writemask,
                zeroing,
                ..
            } => self.emit_v_pshufb_wide(dst, a, idx, bytes, writemask, zeroing),
            IrOp::VPshufb256M { dst, a, addr, .. } => self.emit_v_pshufb256_m(dst, a, addr),
            IrOp::VByteShift {
                dst,
                a,
                bytes,
                right,
                ..
            } => self.emit_v_byte_shift(dst, a, bytes, right),
            IrOp::VShuffle32 { dst, a, imm, .. } => self.emit_v_shuffle32(dst, a, imm),
            IrOp::VBlendW { dst, a, b, imm, .. } => self.emit_v_blend_w(dst, a, b, imm),
            IrOp::VBlendD {
                dst,
                a,
                b,
                imm,
                bytes,
            } => self.emit_v_blend_d(dst, a, b, imm, bytes),
            IrOp::VFma {
                dst,
                x,
                y,
                z,
                prec,
                scalar,
                neg_prod,
                neg_add,
                bytes,
                writemask,
                zeroing,
            } => self.emit_v_fma(
                dst, x, y, z, prec, scalar, neg_prod, neg_add, bytes, writemask, zeroing,
            ),
            IrOp::VFmaM {
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
                writemask,
                zeroing,
            } => self.emit_v_fma_m(
                dst, x, y, z, addr, mem_role, prec, scalar, neg_prod, neg_add, bytes, writemask,
                zeroing,
            ),
            IrOp::VPackWide {
                dst,
                a,
                b,
                from_elem,
                signed,
                bytes,
                ..
            } => self.emit_v_pack_wide(dst, a, b, from_elem, signed, bytes),
            IrOp::VPackWideM {
                dst,
                addr,
                from_elem,
                signed,
                ..
            } => self.emit_v_pack_wide_m(dst, addr, from_elem, signed),
            IrOp::VShuffle32Wide {
                dst,
                a,
                imm,
                bytes,
                writemask,
                zeroing,
                ..
            } => self.emit_v_shuffle32_wide(dst, a, imm, bytes, writemask, zeroing),
            IrOp::VMoveHalf {
                dst,
                src,
                dst_high,
                src_high,
                ..
            } => self.emit_v_move_half(dst, src, dst_high, src_high),
            IrOp::VLoadHalf {
                dst, addr, high, ..
            } => self.emit_v_load_half(dst, addr, high),
            IrOp::VStoreHalf {
                addr, src, high, ..
            } => self.emit_v_store_half(addr, src, high),
            IrOp::VExtractW {
                dst, src, index, ..
            } => self.emit_v_extract_w(dst, src, index),
            IrOp::VExtractLane {
                dst,
                src,
                index,
                size,
                ..
            } => self.emit_v_extract_lane(dst, src, index, size),
            IrOp::VMoveMaskB { dst, src, .. } => self.emit_v_move_mask_b(dst, src),
            IrOp::VMoveMaskFp { dst, src, elem } => self.emit_v_move_mask_fp(dst, src, elem),
            IrOp::VZeroUpper { reg, .. } => self.emit_v_zero_upper(reg),
            IrOp::VZeroUpperAll { clear_low } => self.emit_v_zero_upper_all(*clear_low),
            IrOp::VPshufb { dst, a, idx, .. } => self.emit_v_pshufb(dst, a, idx),
            IrOp::VPshufbM { dst, addr, .. } => self.emit_v_pshufb_m(dst, addr),
            IrOp::VAlignr {
                dst, a, src, imm, ..
            } => self.emit_v_alignr(dst, a, src, imm),
            IrOp::VAlignrM { dst, addr, imm, .. } => self.emit_v_alignr_m(dst, addr, imm),
            IrOp::VAes { dst, a, b, op } => self.emit_v_aes(dst, a, b, op),
            IrOp::VAesM { dst, a, addr, op } => self.emit_v_aes_m(dst, a, addr, op),
            IrOp::VAesImc { dst, src } => self.emit_v_aes_imc(dst, src),
            IrOp::VAesImcM { dst, addr } => self.emit_v_aes_imc_m(dst, addr),
            IrOp::VAesKeygen { dst, src, imm } => self.emit_v_aes_keygen(dst, src, imm),
            IrOp::VAesKeygenM { dst, addr, imm } => self.emit_v_aes_keygen_m(dst, addr, imm),
            IrOp::VSha { dst, a, b, imm, op } => self.emit_v_sha(dst, a, b, imm, op),
            IrOp::VShaM {
                dst,
                a,
                addr,
                imm,
                op,
            } => self.emit_v_sha_m(dst, a, addr, imm, op),
            IrOp::Movq2dq { dst, src_mm } => self.emit_movq2dq(dst, src_mm),
            IrOp::Movdq2q { dst_mm, src_xmm } => self.emit_movdq2q(dst_mm, src_xmm),
            IrOp::VPclmul { dst, a, b, imm } => self.emit_v_pclmul(dst, a, b, imm),
            IrOp::VPclmulM { dst, a, addr, imm } => self.emit_v_pclmul_m(dst, a, addr, imm),
            IrOp::VGfni { dst, a, b, imm, op } => self.emit_v_gfni(dst, a, b, imm, op),
            IrOp::VGfniM {
                dst,
                a,
                addr,
                imm,
                op,
            } => self.emit_v_gfni_m(dst, a, addr, imm, op),
            IrOp::VPsign { dst, a, b, lane } => self.emit_v_psign(dst, a, b, lane),
            IrOp::VPsignM { dst, a, addr, lane } => self.emit_v_psign_m(dst, a, addr, lane),
            IrOp::VShufps { dst, a, b, imm, .. } => self.emit_v_shufps(dst, a, b, imm),
            IrOp::VShuffle16 {
                dst, a, imm, high, ..
            } => self.emit_v_shuffle16(dst, a, imm, high),
            IrOp::VUnpackLow {
                dst,
                a,
                b,
                lane,
                high,
                ..
            } => self.emit_v_unpack_low(dst, a, b, lane, high),
            IrOp::VUnpackLowM {
                dst,
                addr,
                lane,
                high,
                ..
            } => self.emit_v_unpack_low_m(dst, addr, lane, high),
            IrOp::VPackUsWB { dst, a, b, .. } => self.emit_v_pack_us_w_b(dst, a, b),
            IrOp::VPMAddWd { dst, a, b, .. } => self.emit_v_pmaddwd(dst, a, b),
            IrOp::VInsertW {
                dst, src, index, ..
            } => self.emit_v_insert_w(dst, src, index),
            IrOp::VInsertLane {
                dst,
                base,
                src,
                index,
                size,
                ..
            } => self.emit_v_insert_lane(dst, base, src, index, size),
            IrOp::VFloatMov {
                dst, a, src, prec, ..
            } => self.emit_v_float_mov(dst, a, src, prec),
            IrOp::VFloatBin {
                dst,
                a,
                b,
                op,
                prec,
                scalar,
                ..
            } => self.emit_v_float_bin(dst, a, b, op, prec, scalar),
            IrOp::VFloatBinM {
                dst,
                addr,
                op,
                prec,
                scalar,
                ..
            } => self.emit_v_float_bin_m(dst, addr, op, prec, scalar),
            IrOp::VHFloat {
                dst,
                a,
                b,
                op,
                prec,
                ..
            } => self.emit_v_h_float(dst, a, b, op, prec),
            IrOp::VHFloatM {
                dst,
                addr,
                op,
                prec,
                ..
            } => self.emit_v_h_float_m(dst, addr, op, prec),
            IrOp::VHInt { dst, a, b, op, .. } => self.emit_v_h_int(dst, a, b, op),
            IrOp::VHIntM { dst, addr, op, .. } => self.emit_v_h_int_m(dst, addr, op),
            IrOp::VFloatCmp { a, b, prec, .. } => self.emit_v_float_cmp(a, b, prec),
            IrOp::VFloatCmpMask {
                dst,
                a,
                b,
                prec,
                scalar,
                pred,
                ..
            } => self.emit_v_float_cmp_mask(dst, a, b, prec, scalar, pred),
            IrOp::VFloatCmpMaskM {
                dst,
                addr,
                prec,
                scalar,
                pred,
                ..
            } => self.emit_v_float_cmp_mask_m(dst, addr, prec, scalar, pred),
            IrOp::VFloatCmpMask256 {
                dst,
                a,
                b,
                prec,
                pred,
                ..
            } => self.emit_v_float_cmp_mask256(dst, a, b, prec, pred),
            IrOp::VFloatCmpMask256M {
                dst,
                a,
                addr,
                prec,
                pred,
                ..
            } => self.emit_v_float_cmp_mask256_m(dst, a, addr, prec, pred),
            IrOp::VCvtFromInt {
                dst,
                src,
                int_size,
                prec,
                signed,
                ..
            } => self.emit_v_cvt_from_int(dst, src, int_size, prec, signed),
            IrOp::VCvtToInt {
                dst,
                src,
                int_size,
                prec,
                trunc,
                signed,
                ..
            } => self.emit_v_cvt_to_int(dst, src, int_size, prec, trunc, signed),
            IrOp::VCvtFloat {
                dst, src, from, to, ..
            } => self.emit_v_cvt_float(dst, src, from, to),
            IrOp::VPackedCvt { dst, src, kind } => self.emit_v_packed_cvt(dst, src, kind),
            IrOp::VFloatUnary {
                dst,
                a,
                src,
                op,
                prec,
                scalar,
                ..
            } => self.emit_v_float_unary(dst, a, src, op, prec, scalar),
            IrOp::SetDf { value, .. } => self.emit_set_df(value),
            IrOp::RepString {
                op,
                elem,
                rep,
                addr_bits,
                seg_base,
            } => self.emit_rep_string(op, elem, rep, addr_bits, seg_base),
            IrOp::Jump { target, .. } => self.emit_jump(target),
            IrOp::Branch {
                cond,
                taken,
                fallthrough,
                ..
            } => self.emit_branch(cond, taken, fallthrough),
            IrOp::Call {
                target,
                return_addr,
                slot,
                wrap_sp,
                ..
            } => self.emit_call(target, return_addr, slot, wrap_sp),
            IrOp::Ret {
                slot,
                pop_extra,
                wrap_sp,
                ..
            } => self.emit_ret(slot, pop_extra, wrap_sp),
            IrOp::Syscall { is_amd64 } => self.emit_syscall(*is_amd64),
            IrOp::Hlt => self.emit_hlt(),
            IrOp::Trap {
                vector, advance, ..
            } => self.emit_trap(vector, advance),
            IrOp::PortIo { .. } => self.emit_port_io(),
        }
    }

    // --- ALU + flags (mirrors interp::alu_add / alu_sub / alu_logic) ---

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_sub(
        &mut self,
        dst: u32,
        a: Value,
        b: Value,
        cin: Value,
        size: u8,
        mask: FlagMask,
        sub: bool,
    ) {
        let am = self.mask(a, size);
        let bm = self.mask(b, size);

        let (res, cf) = if sub {
            let d = self.builder.ins().isub(am, bm);
            let bor1 = self.builder.ins().icmp(IntCC::UnsignedLessThan, am, bm);
            let res = self.builder.ins().isub(d, cin);
            let bor2 = self.builder.ins().icmp(IntCC::UnsignedLessThan, d, cin);
            let cf = self.builder.ins().bor(bor1, bor2);
            let res = self.mask(res, size);
            (res, cf)
        } else if size >= 8 {
            let s1 = self.builder.ins().iadd(am, bm);
            let c1 = self.builder.ins().icmp(IntCC::UnsignedLessThan, s1, am);
            let res = self.builder.ins().iadd(s1, cin);
            let c2 = self.builder.ins().icmp(IntCC::UnsignedLessThan, res, s1);
            let cf = self.builder.ins().bor(c1, c2);
            (res, cf)
        } else {
            let s0 = self.builder.ins().iadd(am, bm);
            let s = self.builder.ins().iadd(s0, cin);
            let shifted = self.builder.ins().ushr_imm(s, (size * 8) as i64);
            let cf_bit = self.builder.ins().band_imm(shifted, 1);
            let cf = self.builder.ins().ireduce(types::I8, cf_bit);
            let res = self.mask(s, size);
            (res, cf)
        };

        let sb = self.sign_bit(size);
        let zf = self.builder.ins().icmp_imm(IntCC::Equal, res, 0);
        let sfx = self.builder.ins().band_imm(res, sb);
        let sf = self.builder.ins().icmp_imm(IntCC::NotEqual, sfx, 0);
        let pf = self.parity(res);

        // OF: add = ~(a^b) & (a^res); sub = (a^b) & (a^res); sign bit set.
        let axb = self.builder.ins().bxor(am, bm);
        let axr = self.builder.ins().bxor(am, res);
        let of_and = if sub {
            self.builder.ins().band(axb, axr)
        } else {
            let n = self.builder.ins().bnot(axb);
            self.builder.ins().band(n, axr)
        };
        let ofx = self.builder.ins().band_imm(of_and, sb);
        let of = self.builder.ins().icmp_imm(IntCC::NotEqual, ofx, 0);

        // AF from bit 3.
        let an = self.builder.ins().band_imm(am, 0xf);
        let bn = self.builder.ins().band_imm(bm, 0xf);
        let af = if sub {
            let bb = self.builder.ins().iadd(bn, cin);
            self.builder.ins().icmp(IntCC::UnsignedLessThan, an, bb)
        } else {
            let s = self.builder.ins().iadd(an, bn);
            let s = self.builder.ins().iadd(s, cin);
            let x = self.builder.ins().band_imm(s, 0x10);
            self.builder.ins().icmp_imm(IntCC::NotEqual, x, 0)
        };

        self.set(dst, res);
        self.store_flags(mask, cf, pf, af, zf, sf, of);
    }

    /// Shift with count-conditional flags (§16): compute the result always, but
    /// only update flags when the masked count != 0 — mirrors the interpreter.
    pub(crate) fn emit_shift(
        &mut self,
        dst: u32,
        kind: ShiftKind,
        a: Value,
        b: Value,
        size: u8,
        mask: FlagMask,
    ) {
        let vm = self.mask(a, size);
        let cnt = self.shift_count(b, size);
        let res = match kind {
            ShiftKind::Shl => {
                let s = self.builder.ins().ishl(vm, cnt);
                self.mask(s, size)
            }
            ShiftKind::Shr => self.builder.ins().ushr(vm, cnt),
            ShiftKind::Sar => {
                let se = self.sign_extend(vm, size);
                let s = self.builder.ins().sshr(se, cnt);
                self.mask(s, size)
            }
            ShiftKind::Rol => self.rotate(vm, cnt, size, true),
            ShiftKind::Ror => self.rotate(vm, cnt, size, false),
        };
        self.set(dst, res);
        if mask.is_none() {
            return;
        }

        let cont = self.builder.create_block();
        let doflags = self.builder.create_block();
        let iszero = self.builder.ins().icmp_imm(IntCC::Equal, cnt, 0);
        self.builder.ins().brif(iszero, cont, &[], doflags, &[]);
        self.builder.seal_block(doflags);
        self.builder.switch_to_block(doflags);

        let sb = self.sign_bit(size);
        let zero8 = self.builder.ins().iconst(types::I8, 0);
        let (cf, of) = match kind {
            ShiftKind::Shl => {
                // CF = (cnt <= n) ? bit(n-cnt) : 0; OF = msb(res) ^ CF.
                let n = (size * 8) as i64;
                let le = self
                    .builder
                    .ins()
                    .icmp_imm(IntCC::UnsignedLessThanOrEqual, cnt, n);
                let nsub = self.builder.ins().irsub_imm(cnt, n);
                let bit = self.builder.ins().ushr(vm, nsub);
                let bit = self.builder.ins().band_imm(bit, 1);
                let bit = self.builder.ins().ireduce(types::I8, bit);
                let cf = self.builder.ins().select(le, bit, zero8);
                let m = self.builder.ins().band_imm(res, sb);
                let msb = self.builder.ins().icmp_imm(IntCC::NotEqual, m, 0);
                let of = self.builder.ins().bxor(msb, cf);
                (cf, of)
            }
            ShiftKind::Shr => {
                // CF = bit(cnt-1) of the original; OF = original MSB.
                let cm1 = self.builder.ins().iadd_imm(cnt, -1);
                let bit = self.builder.ins().ushr(vm, cm1);
                let bit = self.builder.ins().band_imm(bit, 1);
                let cf = self.builder.ins().ireduce(types::I8, bit);
                let m = self.builder.ins().band_imm(vm, sb);
                let of = self.builder.ins().icmp_imm(IntCC::NotEqual, m, 0);
                (cf, of)
            }
            ShiftKind::Sar => {
                let cm1 = self.builder.ins().iadd_imm(cnt, -1);
                let bit = self.builder.ins().ushr(vm, cm1);
                let bit = self.builder.ins().band_imm(bit, 1);
                let cf = self.builder.ins().ireduce(types::I8, bit);
                (cf, zero8)
            }
            ShiftKind::Rol => {
                // CF = LSB(res); OF = MSB(res) ^ CF.
                let lsb = self.builder.ins().band_imm(res, 1);
                let cf = self.builder.ins().ireduce(types::I8, lsb);
                let m = self.builder.ins().band_imm(res, sb);
                let msb = self.builder.ins().icmp_imm(IntCC::NotEqual, m, 0);
                let of = self.builder.ins().bxor(msb, cf);
                (cf, of)
            }
            ShiftKind::Ror => {
                // CF = MSB(res); OF = MSB(res) ^ bit(n-2).
                let m = self.builder.ins().band_imm(res, sb);
                let cf = self.builder.ins().icmp_imm(IntCC::NotEqual, m, 0);
                let n = (size * 8) as i64;
                let below = self.builder.ins().ushr_imm(res, n - 2);
                let below = self.builder.ins().band_imm(below, 1);
                let below = self.builder.ins().ireduce(types::I8, below);
                let of = self.builder.ins().bxor(cf, below);
                (cf, of)
            }
        };
        let zf = self.builder.ins().icmp_imm(IntCC::Equal, res, 0);
        let sfx = self.builder.ins().band_imm(res, sb);
        let sf = self.builder.ins().icmp_imm(IntCC::NotEqual, sfx, 0);
        let pf = self.parity(res);
        self.store_flags(mask, cf, pf, zero8, zf, sf, of);

        self.builder.ins().jump(cont, &[]);
        self.builder.seal_block(cont);
        self.builder.switch_to_block(cont);
    }

    /// `RCL`/`RCR` — rotate-through-carry (mirrors interp `rcl`/`rcr`). A bit-serial
    /// loop over the effective count `n = (b & countmask) % (size*8 + 1)`, carrying the
    /// value and CF through each step. RCR/RCL are rare (Go's div-by-constant carry
    /// fold, task-132), so a bounded loop over ≤64 iterations is the simplest form that
    /// exactly matches the interpreter and the Unicorn oracle. CF-in comes from the flag
    /// state (like Adc). Flags (CF/OF, count-conditional) set only when `n != 0`.
    pub(crate) fn emit_rcx(
        &mut self,
        dst: u32,
        a: Value,
        b: Value,
        size: u8,
        mask: FlagMask,
        left: bool,
    ) {
        let w = (size as i64) * 8;
        let x = self.mask(a, size);
        let cf_in = self.load_flag_u64(self.offsets.cf); // I64, 0 or 1
        let countmask = if size == 8 { 0x3f } else { 0x1f };
        let nmasked = self.builder.ins().band_imm(b, countmask);
        // Effective count in [0, w]: reduce the masked count mod (width including CF).
        let n = self.builder.ins().urem_imm(nmasked, w + 1);

        // Loop header carries (v, cf, i); it has two preds (entry + body back-edge), so
        // seal it only after the back-edge is emitted.
        let header = self.builder.create_block();
        let body = self.builder.create_block();
        let exit = self.builder.create_block();
        self.builder.append_block_param(header, types::I64); // v
        self.builder.append_block_param(header, types::I64); // cf (0/1)
        self.builder.append_block_param(header, types::I64); // i
        self.builder.append_block_param(exit, types::I64); // final v
        self.builder.append_block_param(exit, types::I64); // final cf

        let zero = self.iconst(0);
        self.builder.ins().jump(header, &[x, cf_in, zero]);
        self.builder.switch_to_block(header);
        let hp = self.builder.block_params(header).to_vec();
        let (v, cf, i) = (hp[0], hp[1], hp[2]);
        let more = self.builder.ins().icmp(IntCC::UnsignedLessThan, i, n);
        self.builder.ins().brif(more, body, &[], exit, &[v, cf]);

        self.builder.switch_to_block(body);
        self.builder.seal_block(body);
        let (nv, ncf) = if left {
            // msb out to CF; shift left, bring old CF into bit 0.
            let top = self.builder.ins().ushr_imm(v, w - 1);
            let msb = self.builder.ins().band_imm(top, 1);
            let sh = self.builder.ins().ishl_imm(v, 1);
            let orr = self.builder.ins().bor(sh, cf);
            (self.mask(orr, size), msb)
        } else {
            // lsb out to CF; shift right, bring old CF into the top bit.
            let lsb = self.builder.ins().band_imm(v, 1);
            let sh = self.builder.ins().ushr_imm(v, 1);
            let cfhi = self.builder.ins().ishl_imm(cf, w - 1);
            (self.builder.ins().bor(sh, cfhi), lsb)
        };
        let ni = self.builder.ins().iadd_imm(i, 1);
        self.builder.ins().jump(header, &[nv, ncf, ni]);
        self.builder.seal_block(header);

        self.builder.switch_to_block(exit);
        self.builder.seal_block(exit);
        let ep = self.builder.block_params(exit).to_vec();
        let (res, cf_out) = (ep[0], ep[1]);
        self.set(dst, res);
        if mask.is_none() {
            return;
        }

        // Flags only when the effective count is non-zero.
        let cont = self.builder.create_block();
        let doflags = self.builder.create_block();
        let iszero = self.builder.ins().icmp_imm(IntCC::Equal, n, 0);
        self.builder.ins().brif(iszero, cont, &[], doflags, &[]);
        self.builder.seal_block(doflags);
        self.builder.switch_to_block(doflags);

        let sb = self.sign_bit(size);
        let cf8 = self.builder.ins().ireduce(types::I8, cf_out);
        let msbm = self.builder.ins().band_imm(res, sb);
        let msb = self.builder.ins().icmp_imm(IntCC::NotEqual, msbm, 0);
        let of = if left {
            // OF = CF-out XOR MSB(result) (defined for count 1).
            self.builder.ins().bxor(msb, cf8)
        } else {
            // OF = XOR of the top two result bits (defined for count 1).
            let below = self.builder.ins().ushr_imm(res, w - 2);
            let below = self.builder.ins().band_imm(below, 1);
            let below = self.builder.ins().ireduce(types::I8, below);
            self.builder.ins().bxor(msb, below)
        };
        let zero8 = self.builder.ins().iconst(types::I8, 0);
        let zf = self.builder.ins().icmp_imm(IntCC::Equal, res, 0);
        let sfx = self.builder.ins().band_imm(res, sb);
        let sf = self.builder.ins().icmp_imm(IntCC::NotEqual, sfx, 0);
        let pf = self.parity(res);
        self.store_flags(mask, cf8, pf, zero8, zf, sf, of);

        self.builder.ins().jump(cont, &[]);
        self.builder.seal_block(cont);
        self.builder.switch_to_block(cont);
    }

    /// `SHLD`/`SHRD` (mirrors interp `DoubleShift`): shift `a` by `count`, filling
    /// the vacated bits from `b`. Masked-count 0 leaves the value and flags unchanged.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_double_shift(
        &mut self,
        dst: u32,
        a: Value,
        b: Value,
        count: Value,
        size: u8,
        left: bool,
        mask: FlagMask,
    ) {
        let va = self.mask(a, size);
        let vb = self.mask(b, size);
        let cnt = self.shift_count(count, size);
        let n = (size * 8) as i64;
        let nsub = self.builder.ins().irsub_imm(cnt, n); // n - cnt
        let shifted = if left {
            let lo = self.builder.ins().ishl(va, cnt);
            let hi = self.builder.ins().ushr(vb, nsub);
            self.builder.ins().bor(lo, hi)
        } else {
            let lo = self.builder.ins().ushr(va, cnt);
            let hi = self.builder.ins().ishl(vb, nsub);
            self.builder.ins().bor(lo, hi)
        };
        let shifted = self.mask(shifted, size);
        // A masked count of 0 is a no-op (and `n - 0` would wrap the shift); keep `a`.
        let iszero = self.builder.ins().icmp_imm(IntCC::Equal, cnt, 0);
        let res = self.builder.ins().select(iszero, va, shifted);
        self.set(dst, res);
        if mask.is_none() {
            return;
        }

        let cont = self.builder.create_block();
        let doflags = self.builder.create_block();
        self.builder.ins().brif(iszero, cont, &[], doflags, &[]);
        self.builder.seal_block(doflags);
        self.builder.switch_to_block(doflags);

        let sb = self.sign_bit(size);
        let zero8 = self.builder.ins().iconst(types::I8, 0);
        // CF = last bit shifted out of `a`: bit(n-cnt) for SHLD, bit(cnt-1) for SHRD.
        let cf = if left {
            let bit = self.builder.ins().ushr(va, nsub);
            let bit = self.builder.ins().band_imm(bit, 1);
            self.builder.ins().ireduce(types::I8, bit)
        } else {
            let cm1 = self.builder.ins().iadd_imm(cnt, -1);
            let bit = self.builder.ins().ushr(va, cm1);
            let bit = self.builder.ins().band_imm(bit, 1);
            self.builder.ins().ireduce(types::I8, bit)
        };
        // OF (count==1): the result's sign bit flipped vs the source's.
        let rm = self.builder.ins().band_imm(res, sb);
        let rmsb = self.builder.ins().icmp_imm(IntCC::NotEqual, rm, 0);
        let am = self.builder.ins().band_imm(va, sb);
        let amsb = self.builder.ins().icmp_imm(IntCC::NotEqual, am, 0);
        let of = self.builder.ins().bxor(rmsb, amsb);
        let zf = self.builder.ins().icmp_imm(IntCC::Equal, res, 0);
        let sfx = self.builder.ins().band_imm(res, sb);
        let sf = self.builder.ins().icmp_imm(IntCC::NotEqual, sfx, 0);
        let pf = self.parity(res);
        self.store_flags(mask, cf, pf, zero8, zf, sf, of);

        self.builder.ins().jump(cont, &[]);
        self.builder.seal_block(cont);
        self.builder.switch_to_block(cont);
    }

    /// Widening multiply (mirrors interp `Mul`). CF=OF set iff the product spills
    /// the low half. For size ≤ 4 the product fits in I64; size 8 uses umulhi/smulhi.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_mul(
        &mut self,
        lo_t: u32,
        hi_t: u32,
        a: Value,
        b: Value,
        size: u8,
        signed: bool,
        mask: FlagMask,
    ) {
        let m = self.mask_imm(size);
        let (lo, hi, overflow) = if size < 8 {
            let n = (size * 8) as i64;
            let (va, vb) = if signed {
                (self.sign_extend(a, size), self.sign_extend(b, size))
            } else {
                (
                    self.builder.ins().band_imm(a, m),
                    self.builder.ins().band_imm(b, m),
                )
            };
            let prod = self.builder.ins().imul(va, vb);
            let lo = self.builder.ins().band_imm(prod, m);
            let hi_sh = self.builder.ins().ushr_imm(prod, n);
            let hi = self.builder.ins().band_imm(hi_sh, m);
            let of = if signed {
                let sl = self.sign_extend(lo, size);
                self.builder.ins().icmp(IntCC::NotEqual, prod, sl)
            } else {
                self.builder.ins().icmp_imm(IntCC::NotEqual, hi, 0)
            };
            (lo, hi, of)
        } else {
            let lo = self.builder.ins().imul(a, b);
            let hi = if signed {
                self.builder.ins().smulhi(a, b)
            } else {
                self.builder.ins().umulhi(a, b)
            };
            let of = if signed {
                let sl = self.builder.ins().sshr_imm(lo, 63);
                self.builder.ins().icmp(IntCC::NotEqual, hi, sl)
            } else {
                self.builder.ins().icmp_imm(IntCC::NotEqual, hi, 0)
            };
            (lo, hi, of)
        };
        self.set(lo_t, lo);
        self.set(hi_t, hi);
        if !mask.is_none() {
            let zero8 = self.builder.ins().iconst(types::I8, 0);
            // CF_OF mask stores only cf and of; pass `overflow` for both.
            self.store_flags(mask, overflow, zero8, zero8, zero8, zero8, overflow);
        }
    }

    /// Divide via the imported helper (§14 #DE). Writes quotient/remainder to a
    /// stack slot; on `#DE` (helper returns nonzero) store RIP and trap out.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_div(
        &mut self,
        quot_t: u32,
        rem_t: u32,
        hi: Value,
        lo: Value,
        divisor: Value,
        size: u8,
        signed: bool,
    ) {
        let sz = self.iconst(size as u64);
        let sg = self.iconst(signed as u64);
        let (ss, inst) = self.call_with_out_slot(self.helpers.div, &[hi, lo, divisor, sz, sg]);
        let de = self.builder.inst_results(inst)[0];

        let ok = self.begin_trap_fork(de);
        let rip = self.iconst(self.cur_addr);
        self.store_cpu(self.offsets.rip, rip);
        // `#DE` is vector 0; publish it so the dispatcher reads a defined vector.
        let vec0 = self.iconst(0);
        self.store_mem(MEMCTX_EXCEPTION_VECTOR, vec0);
        self.ret(RET_EXCEPTION);

        self.builder.switch_to_block(ok);
        let q = self.builder.ins().stack_load(types::I64, ss, 0);
        let r = self.builder.ins().stack_load(types::I64, ss, 8);
        self.set(quot_t, q);
        self.set(rem_t, r);
    }

    pub(crate) fn mask_imm(&self, size: u8) -> i64 {
        if size >= 8 {
            -1
        } else {
            (1i64 << (size * 8)) - 1
        }
    }

    /// Rotate `vm` (masked to `size`) by `cnt`, within the operand width.
    pub(crate) fn rotate(&mut self, vm: Value, cnt: Value, size: u8, left: bool) -> Value {
        if size >= 8 {
            if left {
                self.builder.ins().rotl(vm, cnt)
            } else {
                self.builder.ins().rotr(vm, cnt)
            }
        } else {
            let sv = self.builder.ins().ireduce(int_ty(size), vm);
            let r = if left {
                self.builder.ins().rotl(sv, cnt)
            } else {
                self.builder.ins().rotr(sv, cnt)
            };
            self.builder.ins().uextend(types::I64, r)
        }
    }

    pub(crate) fn logic(&mut self, dst: u32, r: Value, size: u8, mask: FlagMask) {
        let res = self.mask(r, size);
        let zero8 = self.builder.ins().iconst(types::I8, 0);
        let sb = self.sign_bit(size);
        let zf = self.builder.ins().icmp_imm(IntCC::Equal, res, 0);
        let sfx = self.builder.ins().band_imm(res, sb);
        let sf = self.builder.ins().icmp_imm(IntCC::NotEqual, sfx, 0);
        let pf = self.parity(res);
        self.set(dst, res);
        self.store_flags(mask, zero8, pf, zero8, zf, sf, zero8);
    }

    pub(crate) fn parity(&mut self, res: Value) -> Value {
        let low = self.builder.ins().band_imm(res, 0xff);
        let pc = self.builder.ins().popcnt(low);
        let lsb = self.builder.ins().band_imm(pc, 1);
        // Even parity → PF set.
        self.builder.ins().icmp_imm(IntCC::Equal, lsb, 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn store_flags(
        &mut self,
        mask: FlagMask,
        cf: Value,
        pf: Value,
        af: Value,
        zf: Value,
        sf: Value,
        of: Value,
    ) {
        let m = mask.0;
        if m & 0b00_0001 != 0 {
            self.store_flag(self.offsets.cf, cf);
        }
        if m & 0b00_0010 != 0 {
            self.store_flag(self.offsets.pf, pf);
        }
        if m & 0b00_0100 != 0 {
            self.store_flag(self.offsets.af, af);
        }
        if m & 0b00_1000 != 0 {
            self.store_flag(self.offsets.zf, zf);
        }
        if m & 0b01_0000 != 0 {
            self.store_flag(self.offsets.sf, sf);
        }
        if m & 0b10_0000 != 0 {
            self.store_flag(self.offsets.of, of);
        }
    }

    pub(crate) fn eval_cond(&mut self, cond: Cond) -> Value {
        let f = |t: &mut Self, off: i32| t.load_flag(off);
        match cond {
            Cond::Eq => f(self, self.offsets.zf),
            Cond::Ne => {
                let z = f(self, self.offsets.zf);
                self.not(z)
            }
            Cond::Below => f(self, self.offsets.cf),
            Cond::AboveEq => {
                let c = f(self, self.offsets.cf);
                self.not(c)
            }
            Cond::BelowEq => {
                let c = f(self, self.offsets.cf);
                let z = f(self, self.offsets.zf);
                self.builder.ins().bor(c, z)
            }
            Cond::Above => {
                let c = f(self, self.offsets.cf);
                let z = f(self, self.offsets.zf);
                let o = self.builder.ins().bor(c, z);
                self.not(o)
            }
            Cond::Less => {
                let s = f(self, self.offsets.sf);
                let o = f(self, self.offsets.of);
                self.builder.ins().icmp(IntCC::NotEqual, s, o)
            }
            Cond::GreaterEq => {
                let s = f(self, self.offsets.sf);
                let o = f(self, self.offsets.of);
                self.builder.ins().icmp(IntCC::Equal, s, o)
            }
            Cond::LessEq => {
                let s = f(self, self.offsets.sf);
                let o = f(self, self.offsets.of);
                let ne = self.builder.ins().icmp(IntCC::NotEqual, s, o);
                let z = f(self, self.offsets.zf);
                self.builder.ins().bor(ne, z)
            }
            Cond::Greater => {
                let s = f(self, self.offsets.sf);
                let o = f(self, self.offsets.of);
                let eq = self.builder.ins().icmp(IntCC::Equal, s, o);
                let z = f(self, self.offsets.zf);
                let nz = self.not(z);
                self.builder.ins().band(eq, nz)
            }
            Cond::Sign => f(self, self.offsets.sf),
            Cond::NoSign => {
                let s = f(self, self.offsets.sf);
                self.not(s)
            }
            Cond::Overflow => f(self, self.offsets.of),
            Cond::NoOverflow => {
                let o = f(self, self.offsets.of);
                self.not(o)
            }
            Cond::Parity => f(self, self.offsets.pf),
            Cond::NoParity => {
                let p = f(self, self.offsets.pf);
                self.not(p)
            }
        }
    }

    pub(crate) fn not(&mut self, b: Value) -> Value {
        self.builder.ins().bxor_imm(b, 1)
    }

    // --- memory ---

    /// Bounds-check `[addr, addr+size)` against the guest buffer; on failure store
    /// the fault info + RIP and return `RET_UNMAPPED`. Leaves the builder in the
    /// success block and returns the host address `base + addr`.
    pub(crate) fn checked_addr(&mut self, addr: Value, size: u8, access: u64) -> Value {
        // Reuse a bound check already emitted for this exact `(addr, size)` in this
        // block (task-155) — the RMW `Load`+`Store` pair on one effective address. The
        // load's read-fault is what x86 raises first, so skipping the store's check is
        // faithful; the cached host pointer dominates (same straight-line block).
        if let Some(&(_, _, host)) = self
            .checked_ea
            .iter()
            .find(|&&(a, s, _)| a == addr && s == size)
        {
            return host;
        }
        let memsize = self.load_mem(MEMCTX_SIZE);
        let szc = self.iconst(size as u64);
        let end = self.builder.ins().iadd(addr, szc);
        let gt = self
            .builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThan, end, memsize);
        let ov = self.builder.ins().icmp(IntCC::UnsignedLessThan, end, addr);
        let mut oob = self.builder.ins().bor(gt, ov);
        // Identity mapping (§4.1): the guest space is `[guest_base, size)`, so an
        // address below the base has no backing and must trap. Emitted only for a
        // non-zero base — a zero base leaves the check (and the host computation
        // below) byte-identical to the historical zero-based path.
        if self.guest_base != 0 {
            let gb = self.iconst(self.guest_base);
            let below = self.builder.ins().icmp(IntCC::UnsignedLessThan, addr, gb);
            oob = self.builder.ins().bor(oob, below);
        }

        let fault = self.builder.create_block();
        let ok = self.builder.create_block();
        self.builder.ins().brif(oob, fault, &[], ok, &[]);
        self.builder.seal_block(fault);
        self.builder.seal_block(ok);

        self.builder.switch_to_block(fault);
        self.store_mem(MEMCTX_FAULT_ADDR, addr);
        let szc2 = self.iconst(size as u64);
        self.store_mem(MEMCTX_FAULT_SIZE, szc2);
        let acc = self.iconst(access);
        self.store_mem(MEMCTX_FAULT_ACCESS, acc);
        let rip = self.iconst(self.cur_addr);
        self.store_cpu(self.offsets.rip, rip);
        self.ret(RET_UNMAPPED);

        self.builder.switch_to_block(ok);
        // MMIO detection (§5.2, M4-T10): if the VM has Trap regions, an inlined
        // access whose address falls in the baked `[lo, hi)` window is deferred to
        // the interpreter — the block commits nothing of the faulting instruction,
        // sets RIP to it, and returns `RET_MMIO_DEFER`. Nothing emitted when the VM
        // has no MMIO regions (`self.mmio == None`), so the hot path is unchanged.
        if let Some((lo, hi)) = self.mmio {
            let lo_c = self.iconst(lo);
            let hi_c = self.iconst(hi);
            let ge = self
                .builder
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, addr, lo_c);
            let lt = self.builder.ins().icmp(IntCC::UnsignedLessThan, addr, hi_c);
            let in_trap = self.builder.ins().band(ge, lt);
            let defer = self.builder.create_block();
            let cont = self.builder.create_block();
            self.builder.ins().brif(in_trap, defer, &[], cont, &[]);
            self.builder.seal_block(defer);
            self.builder.seal_block(cont);

            self.builder.switch_to_block(defer);
            let rip = self.iconst(self.cur_addr);
            self.store_cpu(self.offsets.rip, rip);
            self.ret(RET_MMIO_DEFER);

            self.builder.switch_to_block(cont);
        }
        let base = self.load_mem(MEMCTX_BASE);
        // `base` is the host address of guest `guest_base`. Rebase the guest address to a
        // backing offset before adding — but only when the base is non-zero, so the zero
        // case emits exactly the historical single `iadd(base, addr)`.
        let host = if self.guest_base == 0 {
            self.builder.ins().iadd(base, addr)
        } else {
            let gb = self.iconst(self.guest_base);
            let off = self.builder.ins().isub(addr, gb);
            self.builder.ins().iadd(base, off)
        };
        self.checked_ea.push((addr, size, host));
        host
    }

    /// Ordinary guest-RAM load at `host` (a host pointer into guest memory),
    /// applying the consistency tier's ordering (§8.2.3). On x86 (native TSO)
    /// this is a plain load in every tier; on ARM, `AcqRel`/`FullTso` add an
    /// acquire fence after the load (blocks Load→Load / Load→Store reordering).
    pub(crate) fn gload(&mut self, ty: Type, host: Value, off: i32) -> Value {
        let v = self.builder.ins().load(ty, MemFlags::trusted(), host, off);
        if cfg!(target_arch = "aarch64") && self.consistency != MemConsistency::Fast {
            self.builder.ins().fence();
        }
        v
    }

    /// Ordinary guest-RAM store of `val` at `host`, applying the tier's ordering.
    /// `AcqRel` fences *before* the store (release — blocks Store→Store /
    /// Load→Store, but permits the Store→Load reorder x86-TSO allows); `FullTso`
    /// fences *after* it as well, additionally blocking Store→Load for full
    /// sequential consistency (the over-strong "hammer"). x86 stays plain.
    pub(crate) fn gstore(&mut self, val: Value, host: Value, off: i32) {
        let arm = cfg!(target_arch = "aarch64");
        if arm && self.consistency == MemConsistency::AcqRel {
            self.builder.ins().fence();
        }
        self.builder
            .ins()
            .store(MemFlags::trusted(), val, host, off);
        if arm && self.consistency == MemConsistency::FullTso {
            self.builder.ins().fence();
        }
    }

    pub(crate) fn load_guest(&mut self, host: Value, size: u8) -> Value {
        let ty = int_ty(size);
        let v = self.gload(ty, host, 0);
        if size < 8 {
            self.builder.ins().uextend(types::I64, v)
        } else {
            v
        }
    }

    pub(crate) fn store_guest(&mut self, host: Value, val: Value, size: u8) {
        let v = if size < 8 {
            self.builder.ins().ireduce(int_ty(size), val)
        } else {
            val
        };
        self.gstore(v, host, 0);
    }

    /// Record an inlined guest store into the embedder's watched data ranges (task-216).
    /// The interpreter does this in `Memory::note_write`; the JIT inlines stores as raw
    /// host writes, so without this a watched range written by JIT'd code would be
    /// invisible to `take_dirty_ranges`. Gated on a LIVE load of `Memory::watch_count`
    /// through the `MemCtx.watch_count_ptr` pointer (task-217) — a pointer load plus a
    /// dependent load of the (shared-clean, L1-cached while unwatched) atomic, then a
    /// never-taken branch. Loading it live rather than from a run-start snapshot means a
    /// watch installed by another thread mid-run is seen on the next store, closing the
    /// multi-vCPU 0→nonzero race. `guest_addr` is the pre-rebase guest address; `size` the
    /// store width.
    pub(crate) fn note_watched_store(&mut self, guest_addr: Value, size: u8) {
        let wcp = self.load_mem(MEMCTX_WATCH_COUNT_PTR); // -> &AtomicUsize watch_count
        let wc = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), wcp, 0); // live count
        let watched = self.builder.ins().icmp_imm(IntCC::NotEqual, wc, 0);
        let doit = self.builder.create_block();
        let cont = self.builder.create_block();
        self.builder.ins().brif(watched, doit, &[], cont, &[]);
        self.builder.seal_block(doit);

        self.builder.switch_to_block(doit);
        let mem_self = self.load_mem(MEMCTX_MEM_SELF);
        let len = self.iconst(size as u64);
        self.call_helper(self.helpers.note_watch, &[mem_self, guest_addr, len]);
        self.builder.ins().jump(cont, &[]);

        self.builder.seal_block(cont);
        self.builder.switch_to_block(cont);
    }

    // --- registers ---

    pub(crate) fn read_reg(&mut self, reg: Reg) -> Value {
        match reg.gpr_index() {
            Some(i) => self.read_gpr(i),
            None => self.load_cpu(self.reg_off(reg)),
        }
    }

    pub(crate) fn read_gpr(&mut self, index: usize) -> Value {
        if let Some(vars) = self.gpr_vars {
            return self.builder.use_var(vars[index]); // region: SSA Variable
        }
        if let Some(v) = self.gpr_cache[index] {
            return v;
        }
        let v = self.load_cpu(self.offsets.gpr(index));
        self.gpr_cache[index] = Some(v);
        v
    }

    pub(crate) fn write_reg(&mut self, reg: Reg, val: Value, size: u8) {
        match reg.gpr_index() {
            Some(i) => self.write_gpr(i, val, size),
            None => self.store_cpu(self.reg_off(reg), val),
        }
    }

    pub(crate) fn write_gpr(&mut self, index: usize, val: Value, size: u8) {
        let off = self.offsets.gpr(index);
        let new = match size {
            8 => val,
            4 => self.builder.ins().band_imm(val, 0xffff_ffff),
            2 => {
                let cur = self.read_gpr(index); // cached current value (no reload)
                let hi = self.builder.ins().band_imm(cur, !0xffffi64);
                let lo = self.builder.ins().band_imm(val, 0xffff);
                self.builder.ins().bor(hi, lo)
            }
            1 => {
                let cur = self.read_gpr(index);
                let hi = self.builder.ins().band_imm(cur, !0xffi64);
                let lo = self.builder.ins().band_imm(val, 0xff);
                self.builder.ins().bor(hi, lo)
            }
            _ => unreachable!("gpr write size 1/2/4/8"),
        };
        if let Some(vars) = self.gpr_vars {
            self.builder.def_var(vars[index], new); // region: stays in a Variable
        } else {
            // Write-through so CpuState is always current, and cache the new value.
            self.store_cpu(off, new);
            self.gpr_cache[index] = Some(new);
        }
    }

    /// Region mode: store every GPR Variable back to `CpuState` (a no-op in
    /// single-block mode, where writes are already write-through). Called before
    /// every exit/trap so the dispatcher and helpers see current guest registers.
    pub(crate) fn flush_gprs(&mut self) {
        if let Some(vars) = self.gpr_vars {
            for (i, &var) in vars.iter().enumerate() {
                let v = self.builder.use_var(var);
                self.store_cpu(self.offsets.gpr(i), v);
            }
        }
        if let Some(fv) = self.fuel_var {
            let v = self.builder.use_var(fv);
            self.store_mem(MEMCTX_FUEL, v); // the dispatcher reads this after the call
        }
    }

    /// Reload the GPRs from `CpuState` after a helper that wrote them (cpuid, x87,
    /// rep-string). Region mode redefines the Variables; single-block mode drops the
    /// value cache so the next read reloads.
    pub(crate) fn reload_gprs(&mut self) {
        if let Some(vars) = self.gpr_vars {
            for (i, &var) in vars.iter().enumerate() {
                let v = self.load_cpu(self.offsets.gpr(i));
                self.builder.def_var(var, v);
            }
        } else {
            self.gpr_cache = [None; 16];
        }
    }

    pub(crate) fn reg_off(&self, reg: Reg) -> i32 {
        match reg {
            Reg::Rip => self.offsets.rip,
            Reg::FsBase => self.offsets.fs_base,
            Reg::GsBase => self.offsets.gs_base,
            _ => unreachable!("non-gpr reg expected"),
        }
    }

    // --- primitives ---

    pub(crate) fn val(&mut self, v: Val) -> Value {
        match v {
            Val::Temp(t) => self.temps[t as usize].expect("temp defined before use"),
            Val::Imm(i) => self.iconst(i),
        }
    }

    pub(crate) fn set(&mut self, dst: u32, v: Value) {
        self.temps[dst as usize] = Some(v);
    }

    pub(crate) fn iconst(&mut self, v: u64) -> Value {
        self.builder.ins().iconst(types::I64, v as i64)
    }

    /// Call an imported Rust helper indirectly through its baked absolute address,
    /// so the compiled block emits no relocation for the call (AOT prerequisite).
    pub(crate) fn call_helper(&mut self, helper: (ir::SigRef, u64), args: &[Value]) -> ir::Inst {
        let (sig, addr) = helper;
        let callee = self.iconst(addr);
        self.builder.ins().call_indirect(sig, callee, args)
    }

    /// Call a helper that writes two i64 results through a trailing out-pointer (div,
    /// bmi): allocate a 16-byte slot, append its address to `args`, and call. Returns
    /// `(slot, call_inst)`; the caller `stack_load`s offsets 0 and 8 where it needs them
    /// (div loads only after its trap fork, so the loads stay caller-placed).
    pub(crate) fn call_with_out_slot(
        &mut self,
        helper: (ir::SigRef, u64),
        args: &[Value],
    ) -> (ir::StackSlot, ir::Inst) {
        let ss = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            16,
            3,
        ));
        let out = self.builder.ins().stack_addr(types::I64, ss, 0);
        let mut full: Vec<Value> = args.to_vec();
        full.push(out);
        let inst = self.call_helper(helper, &full);
        (ss, inst)
    }

    pub(crate) fn mask(&mut self, v: Value, size: u8) -> Value {
        if size >= 8 {
            v
        } else {
            let m = (1i64 << (size * 8)) - 1;
            self.builder.ins().band_imm(v, m)
        }
    }

    pub(crate) fn sign_bit(&self, size: u8) -> i64 {
        1i64 << (size * 8 - 1)
    }

    pub(crate) fn sign_extend(&mut self, v: Value, from: u8) -> Value {
        if from >= 8 {
            return v;
        }
        let shift = (64 - from * 8) as i64;
        let up = self.builder.ins().ishl_imm(v, shift);
        self.builder.ins().sshr_imm(up, shift)
    }

    pub(crate) fn shift_count(&mut self, b: Value, size: u8) -> Value {
        let m = if size == 8 { 63 } else { 31 };
        self.builder.ins().band_imm(b, m)
    }

    pub(crate) fn load_cpu(&mut self, off: i32) -> Value {
        self.builder
            .ins()
            .load(types::I64, MemFlags::trusted(), self.cpu, off)
    }

    pub(crate) fn store_cpu(&mut self, off: i32, v: Value) {
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.cpu, off);
    }

    /// Reinterpret an I128 as a vector type (same bits). Cranelift requires an
    /// endianness for a lane-count-changing bitcast; the guest is little-endian.
    pub(crate) fn bitcast_v(&mut self, v: Value, ty: Type) -> Value {
        let flags = MemFlags::new().with_endianness(ir::Endianness::Little);
        self.builder.ins().bitcast(ty, flags, v)
    }

    pub(crate) fn bitcast_i128(&mut self, v: Value) -> Value {
        let flags = MemFlags::new().with_endianness(ir::Endianness::Little);
        self.builder.ins().bitcast(types::I128, flags, v)
    }

    /// Reinterpret a scalar of the same bit width (int<->float). No lane count
    /// changes, so no endianness specifier is needed.
    pub(crate) fn bitcast_scalar(&mut self, ty: Type, v: Value) -> Value {
        self.builder.ins().bitcast(ty, MemFlags::new(), v)
    }

    /// Keep the low `width` bits of an I64 opmask value (`width` ∈ {8,16,32,64}).
    pub(crate) fn mask_kwidth(&mut self, v: Value, width: u8) -> Value {
        if width >= 64 {
            v
        } else {
            self.builder.ins().band_imm(v, ((1u64 << width) - 1) as i64)
        }
    }

    /// Reduce a 64-bit value to the `size`-byte integer type (no-op at size 8).
    pub(crate) fn narrow(&mut self, v: Value, size: u8) -> Value {
        if size >= 8 {
            v
        } else {
            self.builder.ins().ireduce(int_ty(size), v)
        }
    }

    /// Zero-extend a `size`-byte integer back to I64 (no-op at size 8).
    pub(crate) fn widen(&mut self, v: Value, size: u8) -> Value {
        if size >= 8 {
            v
        } else {
            self.builder.ins().uextend(types::I64, v)
        }
    }

    /// Emit `pshufb`: mask the index bytes to `0x8F` (keep the zero-select bit and
    /// the low nibble) so a set top bit maps to an out-of-range lane, then use
    /// Cranelift's `swizzle` (out-of-range → 0). `data`/`idx` are raw I128.
    pub(crate) fn emit_pshufb(&mut self, data: Value, idx: Value) -> Value {
        let dv = self.bitcast_v(data, types::I8X16);
        let iv = self.bitcast_v(idx, types::I8X16);
        let m = self.builder.ins().iconst(types::I8, 0x8f);
        let mvec = self.builder.ins().splat(types::I8X16, m);
        let masked = self.builder.ins().band(iv, mvec);
        let r = self.builder.ins().swizzle(dv, masked);
        self.bitcast_i128(r)
    }

    /// `palignr`: concatenate `dst` (high 16 bytes) with `src` (low 16), shift the
    /// 32-byte value right by `imm` bytes, keep the low 16. `imm` is a compile-time
    /// constant, so this lowers to at most two i128 shifts + an or (no shift-by-128).
    pub(crate) fn emit_palignr(&mut self, dst: Value, src: Value, imm: u8) -> Value {
        let shift = imm as i64 * 8; // bit shift over the 256-bit concatenation
        if imm >= 32 {
            let z = self.builder.ins().iconst(types::I64, 0);
            self.builder.ins().uextend(types::I128, z)
        } else if shift == 0 {
            src
        } else if shift < 128 {
            let lo = self.builder.ins().ushr_imm(src, shift);
            let hi = self.builder.ins().ishl_imm(dst, 128 - shift);
            self.builder.ins().bor(lo, hi)
        } else if shift == 128 {
            dst
        } else {
            self.builder.ins().ushr_imm(dst, shift - 128)
        }
    }

    /// EVEX `vpcmp{,u}` → opmask (task-168.5). Per 128-bit chunk: vector-compare the
    /// `elem`-lanes, extract one bit per lane with `vhigh_bits`, shift into position,
    /// OR into the k accumulator. FALSE/TRUE predicates skip the compare.
    #[allow(clippy::too_many_arguments)]
    /// EVEX `vptestm`/`vptestnm` → opmask: per lane, `(a & b) != 0` (or `== 0` for
    /// `neg`). Mirrors `emit_vpcmp_to_mask` but tests the AND against zero.
    #[allow(clippy::too_many_arguments)]
    /// `b_host`: when `Some`, the second operand is a memory vector at that (already
    /// bounds-checked) host base — chunk `c` is loaded from `[base + c*16]`; when `None`
    /// it is the register `b` (task-195 memory-source forms).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_vptest_to_mask(
        &mut self,
        k: u8,
        a: u8,
        b: u8,
        b_host: Option<Value>,
        elem: u8,
        width: u16,
        neg: bool,
        writemask: Option<u8>,
    ) {
        let vty = match elem {
            1 => types::I8X16,
            2 => types::I16X8,
            4 => types::I32X4,
            _ => types::I64X2,
        };
        let lanes_per_128 = 16u32 / elem as u32;
        let chunks = width as usize / 16;
        let cc = if neg { IntCC::Equal } else { IntCC::NotEqual };
        let mut acc = self.iconst(0);
        for c in 0..chunks {
            let av = match c {
                0 => self.load_xmm(a),
                1 => self.load_ymm_hi(a),
                n => self.load_zmm_hi(a, n - 2),
            };
            let bv = match b_host {
                Some(host) => self.gload(types::I128, host, (c * 16) as i32),
                None => match c {
                    0 => self.load_xmm(b),
                    1 => self.load_ymm_hi(b),
                    n => self.load_zmm_hi(b, n - 2),
                },
            };
            let va = self.bitcast_v(av, vty);
            let vb = self.bitcast_v(bv, vty);
            let anded = self.builder.ins().band(va, vb);
            let lane_ty = vty.lane_type();
            let zero = self.builder.ins().iconst(lane_ty, 0);
            let zerov = self.builder.ins().splat(vty, zero);
            let cmp = self.builder.ins().icmp(cc, anded, zerov);
            let lane_bits = self.builder.ins().vhigh_bits(types::I32, cmp);
            let wide = self.builder.ins().uextend(types::I64, lane_bits);
            let shifted = self
                .builder
                .ins()
                .ishl_imm(wide, (c as u32 * lanes_per_128) as i64);
            acc = self.builder.ins().bor(acc, shifted);
        }
        if let Some(wk) = writemask {
            let m = self.load_cpu(self.offsets.kmask(wk as usize));
            acc = self.builder.ins().band(acc, m);
        }
        let off = self.offsets.kmask(k as usize);
        self.store_cpu(off, acc);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_vpcmp_to_mask(
        &mut self,
        k: u8,
        a: u8,
        b: u8,
        b_host: Option<Value>,
        elem: u8,
        width: u16,
        pred: u8,
        signed: bool,
        writemask: Option<u8>,
    ) {
        let vty = match elem {
            1 => types::I8X16,
            2 => types::I16X8,
            4 => types::I32X4,
            _ => types::I64X2,
        };
        let lanes_per_128 = 16u32 / elem as u32;
        let chunks = width as usize / 16;
        let cc = match pred & 7 {
            0 => Some(IntCC::Equal),
            4 => Some(IntCC::NotEqual),
            1 => Some(if signed {
                IntCC::SignedLessThan
            } else {
                IntCC::UnsignedLessThan
            }),
            2 => Some(if signed {
                IntCC::SignedLessThanOrEqual
            } else {
                IntCC::UnsignedLessThanOrEqual
            }),
            5 => Some(if signed {
                IntCC::SignedGreaterThanOrEqual
            } else {
                IntCC::UnsignedGreaterThanOrEqual
            }),
            6 => Some(if signed {
                IntCC::SignedGreaterThan
            } else {
                IntCC::UnsignedGreaterThan
            }),
            _ => None, // 3 = FALSE, 7 = TRUE
        };
        let all_true = pred & 7 == 7;
        let mut acc = self.iconst(0);
        for c in 0..chunks {
            let av = match c {
                0 => self.load_xmm(a),
                1 => self.load_ymm_hi(a),
                n => self.load_zmm_hi(a, n - 2),
            };
            let bv = match b_host {
                Some(host) => self.gload(types::I128, host, (c * 16) as i32),
                None => match c {
                    0 => self.load_xmm(b),
                    1 => self.load_ymm_hi(b),
                    n => self.load_zmm_hi(b, n - 2),
                },
            };
            let lane_bits = match cc {
                Some(cc) => {
                    let va = self.bitcast_v(av, vty);
                    let vb = self.bitcast_v(bv, vty);
                    let cmp = self.builder.ins().icmp(cc, va, vb);
                    self.builder.ins().vhigh_bits(types::I32, cmp)
                }
                None if all_true => self
                    .builder
                    .ins()
                    .iconst(types::I32, ((1u64 << lanes_per_128) - 1) as i64),
                None => self.builder.ins().iconst(types::I32, 0),
            };
            let wide = self.builder.ins().uextend(types::I64, lane_bits);
            let shifted = self
                .builder
                .ins()
                .ishl_imm(wide, (c as u32 * lanes_per_128) as i64);
            acc = self.builder.ins().bor(acc, shifted);
        }
        if let Some(wk) = writemask {
            let m = self.load_cpu(self.offsets.kmask(wk as usize));
            acc = self.builder.ins().band(acc, m);
        }
        let off = self.offsets.kmask(k as usize);
        self.store_cpu(off, acc);
    }

    /// `vpermd`: cross-lane 32-bit gather. Spill the 8-dword source to a stack
    /// slot, then load each output dword from `base + (ctrl_lane & 7) * 4`. The
    /// dynamic index rules out `stack_load` (const offset), so compute the address.
    pub(crate) fn emit_vpermd(&mut self, dst: u8, ctrl: u8, src: u8) {
        let ss = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            32,
            4,
        ));
        let slo = self.load_xmm(src);
        let shi = self.load_ymm_hi(src);
        self.builder.ins().stack_store(slo, ss, 0);
        self.builder.ins().stack_store(shi, ss, 16);
        let base = self.builder.ins().stack_addr(types::I64, ss, 0);
        let xclo = self.load_xmm(ctrl);
        let xchi = self.load_ymm_hi(ctrl);
        let clo = self.bitcast_v(xclo, types::I32X4);
        let chi = self.bitcast_v(xchi, types::I32X4);
        let zero = self.builder.ins().iconst(types::I32, 0);
        let mut lo = self.builder.ins().splat(types::I32X4, zero);
        let mut hi = self.builder.ins().splat(types::I32X4, zero);
        let flags = MemFlags::new();
        for i in 0..8u8 {
            let cvec = if i < 4 { clo } else { chi };
            let c = self.builder.ins().extractlane(cvec, i % 4);
            let idx = self.builder.ins().band_imm(c, 7);
            let idx64 = self.builder.ins().uextend(types::I64, idx);
            let off = self.builder.ins().imul_imm(idx64, 4);
            let addr = self.builder.ins().iadd(base, off);
            let v = self.builder.ins().load(types::I32, flags, addr, 0);
            if i < 4 {
                lo = self.builder.ins().insertlane(lo, v, i);
            } else {
                hi = self.builder.ins().insertlane(hi, v, i - 4);
            }
        }
        let rlo = self.bitcast_i128(lo);
        let rhi = self.bitcast_i128(hi);
        self.store_xmm(dst, rlo);
        self.store_ymm_hi(dst, rhi);
    }

    /// Emit a packed integer op on two same-typed vectors.
    /// Packed shift-by-immediate on one 128-bit lane (shared by 128- and 256-bit).
    pub(crate) fn emit_packed_shift_imm(
        &mut self,
        xa: Value,
        imm: u8,
        lane: u8,
        right: bool,
        arith: bool,
    ) -> Value {
        let vty = vec_ty(lane);
        let bits = lane as u32 * 8;
        let over = imm as u32 >= bits; // x86: count >= width is defined
        let va = self.bitcast_v(xa, vty);
        let zero128 = {
            let z = self.iconst(0);
            self.builder.ins().uextend(types::I128, z)
        };
        if !right {
            if over {
                zero128
            } else {
                let amt = self.builder.ins().iconst(types::I32, imm as i64);
                let v = self.builder.ins().ishl(va, amt);
                self.bitcast_i128(v)
            }
        } else if !arith {
            if over {
                zero128
            } else {
                let amt = self.builder.ins().iconst(types::I32, imm as i64);
                let v = self.builder.ins().ushr(va, amt);
                self.bitcast_i128(v)
            }
        } else {
            let n = if over { bits - 1 } else { imm as u32 };
            let amt = self.builder.ins().iconst(types::I32, n as i64);
            let v = self.builder.ins().sshr(va, amt);
            self.bitcast_i128(v)
        }
    }

    /// Zero vector-register lanes `n..4` — the `set_vec` rule that a load/move narrower
    /// than the full ZMM clears the bytes above it.
    pub(crate) fn store_lanes_zeroed_above(&mut self, dst: u8, n: usize) {
        for i in n..4 {
            let z = self.zero_i128();
            self.store_lane(dst, i, z);
        }
    }

    /// Bitwise vector logic on two I128 values (shared by the 128- and 256-bit paths).
    pub(crate) fn emit_vlogic(&mut self, a: Value, b: Value, op: VLogicOp) -> Value {
        match op {
            VLogicOp::Xor => self.builder.ins().bxor(a, b),
            VLogicOp::And => self.builder.ins().band(a, b),
            VLogicOp::Or => self.builder.ins().bor(a, b),
            VLogicOp::Andn => {
                let na = self.builder.ins().bnot(a);
                self.builder.ins().band(na, b)
            }
        }
    }

    /// `pmovzx`/`pmovsx`: bitcast the source to a `from`-byte lane vector, then widen the
    /// low half (`uwiden_low`/`swiden_low`) repeatedly until the lanes are `to` bytes,
    /// zero- or sign-extending. Result bitcast back to an i128.
    pub(crate) fn emit_pmov_extend(&mut self, src: Value, from: u8, to: u8, signed: bool) -> Value {
        let start = match from {
            1 => types::I8X16,
            2 => types::I16X8,
            _ => types::I32X4, // from == 4
        };
        let mut v = self.bitcast_v(src, start);
        let mut w = from;
        while w < to {
            v = if signed {
                self.builder.ins().swiden_low(v)
            } else {
                self.builder.ins().uwiden_low(v)
            };
            w *= 2;
        }
        self.bitcast_i128(v)
    }

    /// SSE4.1 variable blend: build a per-lane all-ones/zero mask from the `lane`-byte
    /// lanes' top bits (arithmetic shift by `bits-1`), then `bitselect` `s`/`d`.
    pub(crate) fn emit_blendv(&mut self, d: Value, s: Value, mask: Value, lane: u8) -> Value {
        let vty = vec_ty(lane);
        let (dv, sv, mv) = (
            self.bitcast_v(d, vty),
            self.bitcast_v(s, vty),
            self.bitcast_v(mask, vty),
        );
        let lanemask = self.builder.ins().sshr_imm(mv, (lane as i64 * 8) - 1);
        let r = self.builder.ins().bitselect(lanemask, sv, dv);
        self.bitcast_i128(r)
    }

    /// SSE4.1 `round`: round the float lanes of `s` with the imm8 `mode`'s Cranelift
    /// `vpopcnt{d,q}` over one 128-bit lane: replace each `lane`-byte element with its
    /// population count. Per-element scalar `popcnt` (universally supported) keeps this off
    /// any AVX512-BITALG legalization path — the op is cold (task-195).
    pub(crate) fn emit_vpopcnt(&mut self, v128: Value, lane: u8) -> Value {
        let vty = vec_ty(lane);
        let vec = self.bitcast_v(v128, vty);
        let mut out = vec;
        for i in 0..(16 / lane) {
            let e = self.builder.ins().extractlane(vec, i);
            let p = self.builder.ins().popcnt(e);
            out = self.builder.ins().insertlane(out, p, i);
        }
        self.bitcast_i128(out)
    }

    /// equivalent. For a scalar op only lane 0 is replaced, keeping `d`'s other lanes.
    pub(crate) fn emit_round(
        &mut self,
        d: Value,
        s: Value,
        prec: FPrec,
        mode: u8,
        scalar: bool,
    ) -> Value {
        let fty = float_vec_ty(prec);
        let sv = self.bitcast_v(s, fty);
        let m = if mode & 4 != 0 { 0 } else { mode & 3 };
        let rounded = match m {
            1 => self.builder.ins().floor(sv),
            2 => self.builder.ins().ceil(sv),
            3 => self.builder.ins().trunc(sv),
            _ => self.builder.ins().nearest(sv),
        };
        if scalar {
            // Keep d's lanes, overwrite lane 0 with the rounded value.
            let dv = self.bitcast_v(d, fty);
            let r0 = self.builder.ins().extractlane(rounded, 0);
            let out = self.builder.ins().insertlane(dv, r0, 0);
            self.bitcast_i128(out)
        } else {
            self.bitcast_i128(rounded)
        }
    }

    /// `vpternlog` over one 128-bit lane: for each of the 8 index combinations whose
    /// `imm` bit is set, OR in `pa & pb & pc` where each polarity is the source (index
    /// bit 1) or its complement (index bit 0). Mirrors the interpreter's `ternlog`.
    pub(crate) fn emit_ternlog(&mut self, a: Value, b: Value, c: Value, imm: u8) -> Value {
        let na = self.builder.ins().bnot(a);
        let nb = self.builder.ins().bnot(b);
        let nc = self.builder.ins().bnot(c);
        let mut acc: Option<Value> = None;
        for j in 0..8u8 {
            if imm & (1 << j) == 0 {
                continue;
            }
            let pa = if j & 4 != 0 { a } else { na };
            let pb = if j & 2 != 0 { b } else { nb };
            let pc = if j & 1 != 0 { c } else { nc };
            let ab = self.builder.ins().band(pa, pb);
            let term = self.builder.ins().band(ab, pc);
            acc = Some(match acc {
                None => term,
                Some(prev) => self.builder.ins().bor(prev, term),
            });
        }
        acc.unwrap_or_else(|| self.zero_i128())
    }

    pub(crate) fn emit_packed_bin(&mut self, a: Value, b: Value, op: PackedBinOp) -> Value {
        match op {
            PackedBinOp::Add => self.builder.ins().iadd(a, b),
            PackedBinOp::Sub => self.builder.ins().isub(a, b),
            PackedBinOp::CmpEq => self.builder.ins().icmp(IntCC::Equal, a, b),
            PackedBinOp::CmpGt => self.builder.ins().icmp(IntCC::SignedGreaterThan, a, b),
            PackedBinOp::MinU => self.builder.ins().umin(a, b),
            PackedBinOp::MaxU => self.builder.ins().umax(a, b),
            PackedBinOp::MinS => self.builder.ins().smin(a, b),
            PackedBinOp::MaxS => self.builder.ins().smax(a, b),
            PackedBinOp::MulLo16 | PackedBinOp::MulLo32 | PackedBinOp::MulLo64 => {
                self.builder.ins().imul(a, b)
            }
            // vpmulhuw/vpmulhw: high 16 bits of each 16×16 product. Cranelift has no vector
            // high-multiply/narrow lowering here, so widen each 16-bit lane to 32, multiply,
            // shift the product right 16 (scalar amount), then gather the low 16 bits of each
            // I32 lane back into an I16x8 with a byte shuffle (low 2 bytes of lanes 0,2,4,6).
            PackedBinOp::MulHiU16 | PackedBinOp::MulHiS16 => {
                let signed = matches!(op, PackedBinOp::MulHiS16);
                let (alo, ahi, blo, bhi) = if signed {
                    (
                        self.builder.ins().swiden_low(a),
                        self.builder.ins().swiden_high(a),
                        self.builder.ins().swiden_low(b),
                        self.builder.ins().swiden_high(b),
                    )
                } else {
                    (
                        self.builder.ins().uwiden_low(a),
                        self.builder.ins().uwiden_high(a),
                        self.builder.ins().uwiden_low(b),
                        self.builder.ins().uwiden_high(b),
                    )
                };
                let plo = self.builder.ins().imul(alo, blo);
                let phi = self.builder.ins().imul(ahi, bhi);
                let sh = self.builder.ins().iconst(types::I32, 16);
                let (plo, phi) = if signed {
                    (
                        self.builder.ins().sshr(plo, sh),
                        self.builder.ins().sshr(phi, sh),
                    )
                } else {
                    (
                        self.builder.ins().ushr(plo, sh),
                        self.builder.ins().ushr(phi, sh),
                    )
                };
                // plo/phi are I32x4 with the result in the low 16 bits of each lane; gather
                // those (bytes 0,1 of lanes 0,2,4,6 = plo, then phi) into one I16x8.
                let plo = self.bitcast_v(plo, types::I8X16);
                let phi = self.bitcast_v(phi, types::I8X16);
                let mask = [0, 1, 4, 5, 8, 9, 12, 13, 16, 17, 20, 21, 24, 25, 28, 29];
                self.shuffle(plo, phi, mask)
            }
            // vpmuludq: mask each 64-bit lane to its low dword, then multiply — both
            // operands < 2^32 so the 64-bit product is exact (matches the interpreter).
            PackedBinOp::MulU32 => {
                let ty = self.builder.func.dfg.value_type(a);
                let lo = self.builder.ins().iconst(ty.lane_type(), 0xffff_ffff);
                let mask = self.builder.ins().splat(ty, lo);
                let am = self.builder.ins().band(a, mask);
                let bm = self.builder.ins().band(b, mask);
                self.builder.ins().imul(am, bm)
            }
            // vpmuldq: sign-extend each 64-bit lane's low dword (shift left 32, arithmetic
            // shift right 32) so the 64-bit product matches the interpreter's signed mul.
            PackedBinOp::MulS32 => {
                // SIMD shifts take a *scalar* shift amount (not a splatted vector).
                let sh = self.builder.ins().iconst(types::I32, 32);
                let al = self.builder.ins().ishl(a, sh);
                let asx = self.builder.ins().sshr(al, sh);
                let bl = self.builder.ins().ishl(b, sh);
                let bsx = self.builder.ins().sshr(bl, sh);
                self.builder.ins().imul(asx, bsx)
            }
            // paddsb/paddsw/paddusb/paddusw/psubsb/psubsw/psubusb/psubusw (task-190):
            // the vector is already lane-typed (I8X16 / I16X8), so the native
            // saturating-arithmetic ops match the interpreter per lane.
            PackedBinOp::AddSatS => self.builder.ins().sadd_sat(a, b),
            PackedBinOp::AddSatU => self.builder.ins().uadd_sat(a, b),
            PackedBinOp::SubSatS => self.builder.ins().ssub_sat(a, b),
            PackedBinOp::SubSatU => self.builder.ins().usub_sat(a, b),
            // pavgb/pavgw (task-190): unsigned rounding average (a + b + 1) >> 1.
            PackedBinOp::AvgU => self.builder.ins().avg_round(a, b),
        }
    }

    /// Emit a scalar or vector float unary op.
    pub(crate) fn emit_funary(&mut self, x: Value, op: FloatUnOp) -> Value {
        match op {
            FloatUnOp::Sqrt => self.builder.ins().sqrt(x),
        }
    }

    /// Emit a scalar or vector float arithmetic op. x86 min/max return the *second*
    /// operand on a NaN or equality, so they lower to an explicit compare+select
    /// (`(a<b)?a:b` / `(a>b)?a:b`) that matches the interpreter bit-for-bit, rather
    /// than an IEEE `fmin`/`fmax` (which differ on NaN).
    pub(crate) fn emit_fbin(&mut self, a: Value, b: Value, op: FloatBinOp) -> Value {
        match op {
            FloatBinOp::Add => self.builder.ins().fadd(a, b),
            FloatBinOp::Sub => self.builder.ins().fsub(a, b),
            FloatBinOp::Mul => self.builder.ins().fmul(a, b),
            FloatBinOp::Div => self.builder.ins().fdiv(a, b),
            FloatBinOp::Min | FloatBinOp::Max => {
                let cc = if matches!(op, FloatBinOp::Min) {
                    FloatCC::LessThan
                } else {
                    FloatCC::GreaterThan
                };
                let cmp = self.builder.ins().fcmp(cc, a, b);
                let ty = self.builder.func.dfg.value_type(a);
                if ty.is_vector() {
                    // fcmp yields an integer lane mask; reinterpret to the float
                    // vector type and bit-select lane-wise.
                    let mask = self.bitcast_v(cmp, ty);
                    self.builder.ins().bitselect(mask, a, b)
                } else {
                    self.builder.ins().select(cmp, a, b)
                }
            }
        }
    }

    /// Byte-permute shuffle of two I8X16 vectors by a compile-time mask (0–15
    /// select from `a`, 16–31 from `b`).
    pub(crate) fn shuffle(&mut self, a: Value, b: Value, mask: [u8; 16]) -> Value {
        let imm = self
            .builder
            .func
            .dfg
            .immediates
            .push(ConstantData::from(mask.as_slice()));
        self.builder.ins().shuffle(a, b, imm)
    }

    pub(crate) fn load_xmm(&mut self, index: u8) -> Value {
        let off = self.offsets.xmm(index as usize);
        self.builder
            .ins()
            .load(types::I128, MemFlags::trusted(), self.cpu, off)
    }

    pub(crate) fn store_xmm(&mut self, index: u8, v: Value) {
        let off = self.offsets.xmm(index as usize);
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.cpu, off);
    }

    /// Load / store the upper 128 bits of YMM `index` (task-168.2).
    pub(crate) fn load_ymm_hi(&mut self, index: u8) -> Value {
        let off = self.offsets.ymm_hi(index as usize);
        self.builder
            .ins()
            .load(types::I128, MemFlags::trusted(), self.cpu, off)
    }

    pub(crate) fn store_ymm_hi(&mut self, index: u8, v: Value) {
        let off = self.offsets.ymm_hi(index as usize);
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.cpu, off);
    }

    /// Bits 511:256 of ZMM `index`; `half` 0 = 383:256, 1 = 511:384 (task-168.5).
    pub(crate) fn load_zmm_hi(&mut self, index: u8, half: usize) -> Value {
        let off = self.offsets.zmm_hi(index as usize, half);
        self.builder
            .ins()
            .load(types::I128, MemFlags::trusted(), self.cpu, off)
    }

    pub(crate) fn store_zmm_hi(&mut self, index: u8, half: usize, v: Value) {
        let off = self.offsets.zmm_hi(index as usize, half);
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.cpu, off);
    }

    /// 128-bit lane `i` (0=xmm, 1=ymm_hi, 2/3=zmm_hi.0/.1) of vector `reg` — the
    /// width-generic accessor for the wide data-mov ops (task-170.2).
    pub(crate) fn load_lane(&mut self, reg: u8, i: usize) -> Value {
        match i {
            0 => self.load_xmm(reg),
            1 => self.load_ymm_hi(reg),
            n => self.load_zmm_hi(reg, n - 2),
        }
    }

    pub(crate) fn store_lane(&mut self, reg: u8, i: usize, v: Value) {
        match i {
            0 => self.store_xmm(reg, v),
            1 => self.store_ymm_hi(reg, v),
            n => self.store_zmm_hi(reg, n - 2, v),
        }
    }

    /// The i128 zero constant (task-170.2).
    pub(crate) fn zero_i128(&mut self) -> Value {
        let z = self.builder.ins().iconst(types::I64, 0);
        self.builder.ins().uextend(types::I128, z)
    }

    /// The block ceremony after a fallible helper call (task-170.4): branch on
    /// `trapped` to a fresh exception block vs an OK block, seal both, and leave the
    /// builder positioned in the **exception** block. Returns the OK block — the caller
    /// emits the exception body (varies: `ret_no_flush` vs store-rip + `ret`), then
    /// `switch_to_block(ok)` and emits the continue path. Keeps both bodies inline (no
    /// closures) while removing the create/brif/seal/switch boilerplate from each site.
    pub(crate) fn begin_trap_fork(&mut self, trapped: Value) -> ir::Block {
        let exc = self.builder.create_block();
        let ok = self.builder.create_block();
        self.builder.ins().brif(trapped, exc, &[], ok, &[]);
        self.builder.seal_block(exc);
        self.builder.seal_block(ok);
        self.builder.switch_to_block(exc);
        ok
    }

    /// Zero the upper 128 bits of YMM `index` (task-168.2) via two 8-byte stores.
    pub(crate) fn store_ymm_hi_zero(&mut self, index: u8) {
        let off = self.offsets.ymm_hi(index as usize);
        let z = self.builder.ins().iconst(types::I64, 0);
        self.builder
            .ins()
            .store(MemFlags::trusted(), z, self.cpu, off);
        self.builder
            .ins()
            .store(MemFlags::trusted(), z, self.cpu, off + 8);
    }

    pub(crate) fn load_mem(&mut self, off: i32) -> Value {
        self.builder
            .ins()
            .load(types::I64, MemFlags::trusted(), self.mem, off)
    }

    pub(crate) fn store_mem(&mut self, off: i32, v: Value) {
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.mem, off);
    }

    pub(crate) fn load_flag(&mut self, off: i32) -> Value {
        self.builder
            .ins()
            .load(types::I8, MemFlags::trusted(), self.cpu, off)
    }

    pub(crate) fn load_flag_u64(&mut self, off: i32) -> Value {
        let b = self.load_flag(off);
        self.builder.ins().uextend(types::I64, b)
    }

    pub(crate) fn store_flag(&mut self, off: i32, v: Value) {
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.cpu, off);
    }

    /// Return to the dispatcher, flushing region GPRs first so `CpuState` is current
    /// (a no-op in single-block mode). Every exit and trap flows through here (incl.
    /// `chain_or_link`), so this one flush covers them all.
    pub(crate) fn ret(&mut self, code: u64) {
        self.flush_gprs();
        self.ret_no_flush(code);
    }

    /// Return WITHOUT flushing — for a helper's own trap path, where the helper has
    /// already written the authoritative `CpuState` (e.g. a partial `rep movs`) and
    /// flushing stale Variables over it would corrupt guest state.
    pub(crate) fn ret_no_flush(&mut self, code: u64) {
        let v = self.iconst(code);
        self.builder.ins().return_(&[v]);
    }

    /// Tail shared by the fault-capable helpers (x87 / fxstate / rep-string): `inst`'s
    /// first result is a status code; if it is `RET_UNMAPPED` the helper already wrote
    /// the authoritative `CpuState` (RIP + fault fields), so fork to the trap path and
    /// return WITHOUT re-flushing. Continues in the ok block otherwise.
    pub(crate) fn trap_if_unmapped(&mut self, inst: ir::Inst) {
        let code = self.builder.inst_results(inst)[0];
        let trapped = self
            .builder
            .ins()
            .icmp_imm(IntCC::Equal, code, RET_UNMAPPED as i64);
        let ok = self.begin_trap_fork(trapped);
        self.ret_no_flush(RET_UNMAPPED);
        self.builder.switch_to_block(ok);
    }

    /// Terminate a direct edge: load the link slot; if filled, hand the next
    /// entry back for a chained transfer, else ask the dispatcher to fill it.
    /// RIP is already stored by the caller.
    pub(crate) fn chain_or_link(&mut self, slot_addr: u64) {
        let slot = self.iconst(slot_addr);
        let entry = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), slot, 0);
        let chain = self.builder.create_block();
        let link = self.builder.create_block();
        self.builder.ins().brif(entry, chain, &[], link, &[]);
        self.builder.seal_block(chain);
        self.builder.seal_block(link);

        self.builder.switch_to_block(chain);
        self.store_mem(MEMCTX_NEXT_ENTRY, entry);
        self.ret(RET_CHAIN);

        self.builder.switch_to_block(link);
        self.store_mem(MEMCTX_LINK_SLOT, slot);
        self.ret(RET_LINK);
    }

    /// Terminate an indirect edge (indirect `jmp`/`call`) with an IBTC probe (R4).
    /// `target` is the computed guest target (already stored to RIP by the caller).
    /// The per-site slot holds either 0 (empty) or a pointer to an immutable
    /// `{cached_target, entry}` descriptor. On a target match, chain straight to the
    /// cached entry (`RET_CHAIN`); otherwise return `RET_IBTC_MISS` with the slot
    /// address so the dispatcher resolves RIP and (re)fills the slot.
    pub(crate) fn ibtc_or_miss(&mut self, slot_addr: u64, target: Value) {
        let slot = self.iconst(slot_addr);
        let desc = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), slot, 0);
        let hit = self.builder.create_block();
        let miss = self.builder.create_block();
        // Empty slot (desc == 0) -> miss; else check the cached target.
        self.builder.ins().brif(desc, hit, &[], miss, &[]);
        self.builder.seal_block(hit);

        self.builder.switch_to_block(hit);
        let cached = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), desc, 0);
        let entry = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), desc, 8);
        let same = self.builder.ins().icmp(IntCC::Equal, cached, target);
        let chain = self.builder.create_block();
        // Target mismatch falls into the same `miss` block (second predecessor).
        self.builder.ins().brif(same, chain, &[], miss, &[]);
        self.builder.seal_block(chain);
        self.builder.seal_block(miss);

        self.builder.switch_to_block(chain);
        self.store_mem(MEMCTX_NEXT_ENTRY, entry);
        self.ret(RET_CHAIN);

        self.builder.switch_to_block(miss);
        self.store_mem(MEMCTX_LINK_SLOT, slot);
        self.ret(RET_IBTC_MISS);
    }

    /// Load this vcpu's shadow return stack pointer from `MemCtx` (R5).
    pub(crate) fn ret_stack_ptr(&mut self) -> Value {
        self.builder
            .ins()
            .load(types::I64, MemFlags::trusted(), self.mem, MEMCTX_RET_STACK)
    }

    /// Byte address of ring frame `sp & (LEN-1)` given the ring base and a `sp`.
    pub(crate) fn ret_frame_addr(&mut self, rs: Value, sp: Value) -> Value {
        let idx = self.builder.ins().band_imm(sp, (RET_STACK_LEN - 1) as i64);
        let stride = self.builder.ins().imul_imm(idx, RETSTACK_STRIDE as i64);
        let off = self.builder.ins().iadd_imm(stride, RETSTACK_ENTRIES as i64);
        self.builder.ins().iadd(rs, off)
    }

    /// Push a predicted return frame `(return_addr, cont_slot_addr)` onto the shadow
    /// ring (R5). Wrap-and-overwrite on overflow — a lost frame only costs a later
    /// misprediction, never a wrong transfer.
    pub(crate) fn emit_ret_push(&mut self, return_addr: u64, cont_slot_addr: u64) {
        let rs = self.ret_stack_ptr();
        let sp = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), rs, RETSTACK_SP);
        let addr = self.ret_frame_addr(rs, sp);
        let ra = self.iconst(return_addr);
        let cs = self.iconst(cont_slot_addr);
        self.builder.ins().store(MemFlags::trusted(), ra, addr, 0);
        self.builder
            .ins()
            .store(MemFlags::trusted(), cs, addr, RETSTACK_STRIDE / 2);
        let sp1 = self.builder.ins().iadd_imm(sp, 1);
        self.builder
            .ins()
            .store(MemFlags::trusted(), sp1, rs, RETSTACK_SP);
    }

    /// Terminate a `ret` with return-address prediction (R5). `actual` is the real
    /// guest return target (already popped off the guest stack and stored to RIP).
    /// Pop the shadow ring; if the frame's predicted address equals `actual`, chain
    /// to the caller's continuation via its slot (filled → `RET_CHAIN`, empty →
    /// `RET_LINK`); on underflow or a mismatch, fall back to `RET_CONTINUE`.
    /// Correctness never depends on the ring — only the addr compare gates a hit.
    pub(crate) fn emit_ret_predict(&mut self, actual: Value) {
        let rs = self.ret_stack_ptr();
        let sp = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), rs, RETSTACK_SP);
        let empty = self.builder.ins().icmp_imm(IntCC::Equal, sp, 0);
        let has = self.builder.create_block();
        let underflow = self.builder.create_block();
        self.builder.ins().brif(empty, underflow, &[], has, &[]);
        self.builder.seal_block(has);
        self.builder.seal_block(underflow);

        // Non-empty: pop and compare.
        self.builder.switch_to_block(has);
        let spdec = self.builder.ins().iadd_imm(sp, -1);
        self.builder
            .ins()
            .store(MemFlags::trusted(), spdec, rs, RETSTACK_SP);
        let addr = self.ret_frame_addr(rs, spdec);
        let pred = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), addr, 0);
        let cont_slot =
            self.builder
                .ins()
                .load(types::I64, MemFlags::trusted(), addr, RETSTACK_STRIDE / 2);
        let same = self.builder.ins().icmp(IntCC::Equal, pred, actual);
        let hit = self.builder.create_block();
        let miss = self.builder.create_block();
        self.builder.ins().brif(same, hit, &[], miss, &[]);
        self.builder.seal_block(hit);
        self.builder.seal_block(miss);

        // Prediction matched: chain through the continuation slot (fill it if cold).
        self.builder.switch_to_block(hit);
        let slotval = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), cont_slot, 0);
        let chain = self.builder.create_block();
        let link = self.builder.create_block();
        self.builder.ins().brif(slotval, chain, &[], link, &[]);
        self.builder.seal_block(chain);
        self.builder.seal_block(link);
        self.builder.switch_to_block(chain);
        self.store_mem(MEMCTX_NEXT_ENTRY, slotval);
        self.ret(RET_CHAIN);
        self.builder.switch_to_block(link);
        self.store_mem(MEMCTX_LINK_SLOT, cont_slot);
        self.ret(RET_LINK);

        // Mispredict or underflow: the plain dispatch path.
        self.builder.switch_to_block(miss);
        self.ret(RET_CONTINUE);
        self.builder.switch_to_block(underflow);
        self.ret(RET_CONTINUE);
    }

    /// Fuel gate before a region sub-block (§12 M5-T3): if `MemCtx.fuel` is spent,
    /// store RIP = `block_addr` and return to the dispatcher (which re-enters there
    /// with the next quantum); otherwise charge one block and fall through. Every
    /// sub-block (including the first — where the dispatcher guarantees fuel ≥ 1, so
    /// the exit is never taken) charges exactly once, so `quantum - fuel` equals the
    /// number of guest blocks run.
    pub(crate) fn emit_fuel_gate(&mut self, block_addr: u64) {
        let fv = self.fuel_var.expect("fuel Variable in region mode");
        let fuel = self.builder.use_var(fv);
        let spent = self.builder.ins().icmp_imm(IntCC::Equal, fuel, 0);
        let exit = self.builder.create_block();
        let cont = self.builder.create_block();
        self.builder.ins().brif(spent, exit, &[], cont, &[]);
        // Blocks are sealed en masse by `translate_region` after the whole CFG is
        // built (a DAG/loop may add predecessors as later blocks are emitted).

        self.builder.switch_to_block(exit);
        let rip = self.iconst(block_addr);
        self.store_cpu(self.offsets.rip, rip);
        self.ret(RET_CONTINUE); // flushes the fuel Variable back to MemCtx

        self.builder.switch_to_block(cont);
        let dec = self.builder.ins().iadd_imm(fuel, -1);
        self.builder.def_var(fv, dec); // stays in a register across the loop
    }

    /// Translate one region sub-block's body and its (region-aware) terminator.
    /// `clif` maps guest addresses to their Cranelift block. A branch/jump to any
    /// in-region block becomes an internal edge — including a **back-edge**, which
    /// makes a guest loop a real host loop (M5-T3d); the fuel gate at each block
    /// entry keeps it preemptible (§9.2). Edges leaving the region take the normal
    /// chain/link exit.
    pub(crate) fn emit_region_block(&mut self, block: &IrBlock, clif: &HashMap<u64, Block>) {
        let internal_term = matches!(
            block.ops.last(),
            Some(IrOp::Branch { .. })
                | Some(IrOp::Jump {
                    target: Val::Imm(_)
                })
        );
        // Body = every op but a branch/jump we handle ourselves; a non-static
        // terminator (call/ret/syscall/hlt/indirect) is translated normally and
        // ends the block via its own exit.
        let body = if internal_term {
            &block.ops[..block.ops.len() - 1]
        } else {
            &block.ops[..]
        };
        for op in body {
            if self.op(op) {
                return; // a normal terminator handled the exit
            }
        }
        match block.ops.last() {
            Some(IrOp::Branch {
                cond,
                taken,
                fallthrough,
            }) => {
                let c = self.eval_cond(*cond);
                let (tk, tk_exit) = self.region_edge(*taken, clif);
                let (fl, fl_exit) = self.region_edge(*fallthrough, clif);
                self.builder.ins().brif(c, tk, &[], fl, &[]);
                if let Some(a) = tk_exit {
                    self.fill_region_exit(tk, a);
                }
                if let Some(a) = fl_exit {
                    self.fill_region_exit(fl, a);
                }
            }
            Some(IrOp::Jump {
                target: Val::Imm(target),
            }) => {
                let (dst, exit) = self.region_edge(*target, clif);
                self.builder.ins().jump(dst, &[]);
                if let Some(a) = exit {
                    self.fill_region_exit(dst, a);
                }
            }
            // No internal-capable terminator ran and the body didn't exit: flow past.
            _ => {
                let end = self.iconst(self.guest_end);
                self.store_cpu(self.offsets.rip, end);
                self.ret(RET_CONTINUE);
            }
        }
    }

    /// Resolve a static edge target to a Cranelift block: the in-region block for an
    /// internal edge (any forward/merge/back edge; returns `None` — no fill needed),
    /// or a fresh exit stub for an out-of-region target (returns `Some(target)`).
    pub(crate) fn region_edge(
        &mut self,
        target: u64,
        clif: &HashMap<u64, Block>,
    ) -> (Block, Option<u64>) {
        match clif.get(&target) {
            Some(&b) => (b, None),                               // internal edge
            None => (self.builder.create_block(), Some(target)), // exit stub, filled by the caller
        }
    }

    /// Fill an exit stub: store RIP and chain/link out to `target_addr`.
    pub(crate) fn fill_region_exit(&mut self, stub: Block, target_addr: u64) {
        self.builder.switch_to_block(stub);
        let rip = self.iconst(target_addr);
        self.store_cpu(self.offsets.rip, rip);
        let slot = (self.alloc_slot)();
        self.chain_or_link(slot);
    }
}

#[derive(Copy, Clone)]
enum ShiftKind {
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
}

/// Stable integer encoding of [`HFloatOp`] passed to the `hfloat` JIT helper (task-244).
fn hfloat_op_code(op: HFloatOp) -> u64 {
    match op {
        HFloatOp::HAdd => 0,
        HFloatOp::HSub => 1,
        HFloatOp::AddSub => 2,
    }
}

/// Stable integer encoding of [`HIntOp`] passed to the `hint` JIT helper (task-247).
/// Must match `hint_op_from_code` in the interpreter.
fn hint_op_code(op: HIntOp) -> u64 {
    match op {
        HIntOp::AddW => 0,
        HIntOp::AddD => 1,
        HIntOp::AddSw => 2,
        HIntOp::SubW => 3,
        HIntOp::SubD => 4,
        HIntOp::SubSw => 5,
        HIntOp::Sad => 6, // task-249: psadbw / vpsadbw
    }
}

/// Byte-permute mask for punpckl* at `lane`-byte element granularity: interleave
/// the low 8 bytes of `a` (0–15) and `b` (16–31).
fn unpack_low_mask(lane: u8, high: bool) -> [u8; 16] {
    let mut mask = [0u8; 16];
    let n = 8 / lane; // elements per half
    let base = if high { n * lane } else { 0 }; // byte offset of the source half
    let mut out = 0usize;
    for k in 0..n {
        for j in 0..lane {
            mask[out] = base + k * lane + j; // a element k, byte j
            out += 1;
        }
        for j in 0..lane {
            mask[out] = 16 + base + k * lane + j; // b element k
            out += 1;
        }
    }
    mask
}

/// 128-bit vector type for a packed op with `lane`-byte elements.
fn vec_ty(lane: u8) -> Type {
    match lane {
        1 => types::I8X16,
        2 => types::I16X8,
        4 => types::I32X4,
        _ => types::I64X2,
    }
}

/// (scalar element type, 128-bit vector type) for a `vpbroadcast` element size.
fn broadcast_types(elem: u8) -> (Type, Type) {
    match elem {
        1 => (types::I8, types::I8X16),
        2 => (types::I16, types::I16X8),
        4 => (types::I32, types::I32X4),
        _ => (types::I64, types::I64X2),
    }
}

/// 128-bit float vector type (`F32X4`/`F64X2`) for a given precision.
fn float_vec_ty(prec: FPrec) -> Type {
    match prec {
        FPrec::F32 => types::F32X4,
        FPrec::F64 => types::F64X2,
    }
}

/// Scalar float type for a given precision.
fn scalar_fty(prec: FPrec) -> Type {
    match prec {
        FPrec::F32 => types::F32,
        FPrec::F64 => types::F64,
    }
}

/// Integer vector type matching a float precision's lanes (for lane-preserving
/// integer inserts of float bits).
fn lane_int_vec_ty(prec: FPrec) -> Type {
    match prec {
        FPrec::F32 => types::I32X4,
        FPrec::F64 => types::I64X2,
    }
}

/// Map an IR `RmwOp` to Cranelift's atomic RMW opcode.
fn rmw_op(op: RmwOp) -> ir::AtomicRmwOp {
    match op {
        RmwOp::Add => ir::AtomicRmwOp::Add,
        RmwOp::Sub => ir::AtomicRmwOp::Sub,
        RmwOp::And => ir::AtomicRmwOp::And,
        RmwOp::Or => ir::AtomicRmwOp::Or,
        RmwOp::Xor => ir::AtomicRmwOp::Xor,
        RmwOp::Xchg => ir::AtomicRmwOp::Xchg,
        // Rsub has no native atomic; the AtomicRmw arm emits a CAS loop for it.
        RmwOp::Rsub => unreachable!("Rsub is lowered as a CAS loop, not a native rmw"),
    }
}

/// Scalar integer type holding one lane's float bits.
fn lane_int_ty(prec: FPrec) -> Type {
    match prec {
        FPrec::F32 => types::I32,
        FPrec::F64 => types::I64,
    }
}

/// Encodings shared with the string helper's decode arrays.
fn str_op_code(op: StrOp) -> u64 {
    match op {
        StrOp::Movs => 0,
        StrOp::Stos => 1,
        StrOp::Scas => 2,
        StrOp::Cmps => 3,
        StrOp::Lods => 4,
    }
}

fn rep_code(rep: RepKind) -> u64 {
    match rep {
        RepKind::None => 0,
        RepKind::Rep => 1,
        RepKind::Repe => 2,
        RepKind::Repne => 3,
    }
}

fn int_ty(size: u8) -> Type {
    match size {
        1 => types::I8,
        2 => types::I16,
        4 => types::I32,
        8 => types::I64,
        _ => unreachable!("access size 1/2/4/8"),
    }
}

// Runs only on an aarch64 host: Cranelift builds just the host ISA backend, so
// the aarch64 lowering is available for inspection only when we *are* aarch64 —
// which is exactly the ARM CI runner this is meant to cover.
#[cfg(all(test, target_arch = "aarch64"))]
mod barrier_tests {
    //! Deterministic proof that the `MemConsistency` tiers actually emit the
    //! x86-TSO barriers on an ARM host (§8.2.3): compile a guest store+load under
    //! each tier and inspect the emitted machine code — no probabilistic race
    //! needed. `fence()` lowers to `DMB ISH` (`0xD5033BBF`) on aarch64; count them.
    use super::{translate_block, Helpers};
    use cranelift::codegen::ir::Signature;
    use cranelift::codegen::{isa, settings, Context};
    use cranelift::prelude::*;
    use x86jit_core::jit_abi::cpu_offsets;
    use x86jit_core::{IrBlock, IrOp, MemConsistency, MemOrder, Val};

    /// `DMB ISH`, little-endian — what Cranelift `fence()` emits on aarch64.
    const DMB_ISH: [u8; 4] = [0xBF, 0x3B, 0x03, 0xD5];

    /// Compile a one-store/one-load block for aarch64 under `tier`; count `DMB`s.
    fn dmb_count(tier: MemConsistency) -> usize {
        let mut fb = settings::builder();
        fb.set("is_pic", "false").unwrap();
        let isa = isa::lookup("aarch64-unknown-linux-gnu".parse().unwrap())
            .unwrap()
            .finish(settings::Flags::new(fb))
            .unwrap();

        let mut ctx = Context::new();
        ctx.func.signature.params.push(AbiParam::new(types::I64)); // cpu
        ctx.func.signature.params.push(AbiParam::new(types::I64)); // mem
        ctx.func.signature.returns.push(AbiParam::new(types::I64));

        // Distinct addresses so the load isn't store-forwarded away, and the
        // loaded value is stored back so it isn't dead-code-eliminated — either
        // would drop a fence and make the count wrong. Two stores + one load.
        let ir = IrBlock {
            ops: vec![
                IrOp::Store {
                    addr: Val::Imm(0x2000),
                    src: Val::Imm(42),
                    size: 8,
                    order: MemOrder::None,
                },
                IrOp::Load {
                    dst: 0,
                    addr: Val::Imm(0x3000),
                    size: 8,
                },
                IrOp::Store {
                    addr: Val::Imm(0x4000),
                    src: Val::Temp(0),
                    size: 8,
                    order: MemOrder::None,
                },
            ],
            temp_count: 1,
            guest_start: 0,
            guest_len: 1,
            icount: 1,
        };
        let offsets = cpu_offsets();

        let mut fbctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fbctx);

        // `note_watched_store` emits a real call to `note_watch` for EVERY store
        // (task-216), so unlike the never-called dummies below its signature must match
        // the actual helper: note_watch(mem_self, addr, len) -> () — 3 params, no return.
        let note_watch = {
            let mut sig = Signature::new(isa.default_call_conv());
            for _ in 0..3 {
                sig.params.push(AbiParam::new(types::I64));
            }
            (builder.import_signature(sig), 0u64)
        };

        // Dummy helper signatures — unused by a plain load/store block, but the
        // signature of `translate_block` requires them. Address 0: never called.
        let mut mk = || {
            let mut sig = Signature::new(isa.default_call_conv());
            for _ in 0..6 {
                sig.params.push(AbiParam::new(types::I64));
            }
            sig.returns.push(AbiParam::new(types::I64));
            (builder.import_signature(sig), 0u64)
        };
        let helpers = Helpers {
            div: mk(),
            string: mk(),
            cpuid: mk(),
            xgetbv: mk(),
            vmaskmov: mk(),
            vmaskmov_mem: mk(),
            vmasked_logic: mk(),
            valign: mk(),
            vpermt2: mk(),
            vpermt2_mem: mk(),
            vperm1: mk(),
            vperm1_mem: mk(),
            vpmov_narrow: mk(),
            vpmov_narrow_mem: mk(),
            vpmov_extend_wide: mk(),
            vpabs: mk(),
            vp_unary_lane: mk(),
            vp_blendm: mk(),
            vshuf_lane: mk(),
            vp_multishift: mk(),
            vpshufb_wide: mk(),
            vshuffle32_wide: mk(),
            vpack: mk(),
            vpack_mem: mk(),
            vhfloat: mk(),
            vhfloat_mem: mk(),
            vhint: mk(),
            vhint_mem: mk(),
            pmaddwd: mk(),
            fma: mk(),
            fma_mem: mk(),
            broadcast_lane: mk(),
            broadcast_lane_mem: mk(),
            aes: mk(),
            aes_mem: mk(),
            sha: mk(),
            sha_mem: mk(),
            gfni: mk(),
            gfni_mem: mk(),
            pclmul: mk(),
            pclmul_mem: mk(),
            mmx_bridge: mk(),
            vmasked_packed: mk(),
            vmasked_shift: mk(),
            var_shift: mk(),
            shift_reg: mk(),
            gf2p8: mk(),
            gf2p8_mem: mk(),
            pcmpstr: mk(),
            pcmpstr_mem: mk(),
            pcmpstrm: mk(),
            pcmpstrm_mem: mk(),
            bmi: mk(),
            x87: mk(),
            fxstate: mk(),
            crc32: mk(),
            note_watch,
        };

        let mut slot = 0u64;
        let mut alloc = || {
            slot += 1;
            slot
        };
        translate_block(
            &mut builder,
            &ir,
            &offsets,
            &mut alloc,
            helpers,
            tier,
            None,
            0,
        );
        builder.finalize();

        let code = ctx.compile(&*isa, &mut Default::default()).unwrap();
        code.code_buffer()
            .windows(4)
            .filter(|w| *w == DMB_ISH)
            .count()
    }

    #[test]
    fn tiers_emit_the_right_aarch64_barriers() {
        // Two stores + one load. Fast: bare LDR/STR, no barriers.
        assert_eq!(dmb_count(MemConsistency::Fast), 0, "Fast must emit no DMB");
        // AcqRel: release before each store + acquire after the load = 3.
        assert_eq!(
            dmb_count(MemConsistency::AcqRel),
            3,
            "AcqRel: a release DMB per store and an acquire DMB after the load"
        );
        // FullTso: a DMB after each store + after the load = 3.
        assert_eq!(
            dmb_count(MemConsistency::FullTso),
            3,
            "FullTso: a DMB per store and after the load"
        );
    }
}
