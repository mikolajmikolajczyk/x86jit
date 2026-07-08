//! Translate an `IrBlock` to Cranelift IR (§8.2.3). One `match` on `IrOp`, but
//! describing operations to a `FunctionBuilder` instead of executing them. Flag
//! computation mirrors the interpreter (`interp.rs`) exactly so the two backends
//! agree bit-for-bit (the M4 acceptance oracle).

use std::collections::HashMap;

use cranelift::prelude::*;

use cranelift::codegen::ir::{self, ConstantData, StackSlotData, StackSlotKind};

use x86jit_core::jit_abi::{
    CpuOffsets, MEMCTX_BASE, MEMCTX_FAULT_ACCESS, MEMCTX_FAULT_ADDR, MEMCTX_FAULT_SIZE,
    MEMCTX_FUEL, MEMCTX_LINK_SLOT, MEMCTX_NEXT_ENTRY, MEMCTX_RET_STACK, MEMCTX_SIZE,
    RETSTACK_ENTRIES, RETSTACK_SP, RETSTACK_STRIDE, RET_CHAIN, RET_CONTINUE, RET_EXCEPTION,
    RET_HLT, RET_IBTC_MISS, RET_LINK, RET_MMIO_DEFER, RET_STACK_LEN, RET_SYSCALL, RET_UNMAPPED,
};
use x86jit_core::{
    BtOp, Cond, FPrec, FlagMask, FloatBinOp, FloatUnOp, IrBlock, IrOp, IrRegion, MemConsistency,
    PackedBinOp, Reg, RepKind, RmwOp, StrOp, VLogicOp, Val,
};

const RSP: usize = 4;

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
    pub x87: (ir::SigRef, u64),
    pub fxstate: (ir::SigRef, u64),
    pub crc32: (ir::SigRef, u64),
}

pub fn translate_block(
    builder: &mut FunctionBuilder,
    ir: &IrBlock,
    offsets: &CpuOffsets,
    alloc_slot: &mut dyn FnMut() -> u64,
    helpers: Helpers,
    consistency: MemConsistency,
    mmio: Option<(u64, u64)>,
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
pub fn translate_region(
    builder: &mut FunctionBuilder,
    region: &IrRegion,
    offsets: &CpuOffsets,
    alloc_slot: &mut dyn FnMut() -> u64,
    helpers: Helpers,
    consistency: MemConsistency,
    mmio: Option<(u64, u64)>,
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
            IrOp::InsnStart { guest_addr } => {
                self.cur_addr = *guest_addr;
                // GP-3 (doc-30): tag the machine code emitted for this guest
                // instruction with its guest RIP, so a guard-page SIGSEGV can map
                // the faulting host PC back to a precise guest RIP via the srcloc
                // side table. Guest code lives below the 4 GiB CODE_WINDOW, so the
                // `u32` SourceLoc is lossless. Emits zero instructions.
                self.builder
                    .set_srcloc(ir::SourceLoc::new(*guest_addr as u32));
                false
            }
            IrOp::ReadReg { dst, reg } => {
                let v = self.read_reg(*reg);
                self.set(*dst, v);
                false
            }
            IrOp::WriteReg { reg, src, size } => {
                let v = self.val(*src);
                self.write_reg(*reg, v, *size);
                false
            }

            IrOp::Add {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let zero = self.iconst(0);
                self.add_sub(*dst, a, b, zero, *size, *set_flags, false);
                false
            }
            IrOp::Adc {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let cin = self.load_flag_u64(self.offsets.cf);
                self.add_sub(*dst, a, b, cin, *size, *set_flags, false);
                false
            }
            IrOp::Sub {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let zero = self.iconst(0);
                self.add_sub(*dst, a, b, zero, *size, *set_flags, true);
                false
            }
            IrOp::Sbb {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let cin = self.load_flag_u64(self.offsets.cf);
                self.add_sub(*dst, a, b, cin, *size, *set_flags, true);
                false
            }
            IrOp::And {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let r = self.builder.ins().band(a, b);
                self.logic(*dst, r, *size, *set_flags);
                false
            }
            IrOp::Or {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let r = self.builder.ins().bor(a, b);
                self.logic(*dst, r, *size, *set_flags);
                false
            }
            IrOp::Xor {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let r = self.builder.ins().bxor(a, b);
                self.logic(*dst, r, *size, *set_flags);
                false
            }

            IrOp::Shl {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Shl, a, b, *size, *set_flags);
                false
            }
            IrOp::Shr {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Shr, a, b, *size, *set_flags);
                false
            }
            IrOp::Sar {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Sar, a, b, *size, *set_flags);
                false
            }
            IrOp::Rol {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Rol, a, b, *size, *set_flags);
                false
            }
            IrOp::Ror {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Ror, a, b, *size, *set_flags);
                false
            }
            IrOp::Rcl {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_rcx(*dst, a, b, *size, *set_flags, true);
                false
            }
            IrOp::Rcr {
                dst,
                a,
                b,
                size,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_rcx(*dst, a, b, *size, *set_flags, false);
                false
            }
            IrOp::DoubleShift {
                dst,
                a,
                b,
                count,
                size,
                left,
                set_flags,
            } => {
                let (a, b, count) = (self.val(*a), self.val(*b), self.val(*count));
                self.emit_double_shift(*dst, a, b, count, *size, *left, *set_flags);
                false
            }
            IrOp::Sext { dst, a, from } => {
                let a = self.val(*a);
                let r = self.sign_extend(a, *from);
                self.set(*dst, r);
                false
            }
            IrOp::Bswap { dst, a, size } => {
                let a = self.val(*a);
                let r = if *size >= 8 {
                    self.builder.ins().bswap(a)
                } else {
                    let s = self.builder.ins().ireduce(int_ty(*size), a);
                    let sw = self.builder.ins().bswap(s);
                    self.builder.ins().uextend(types::I64, sw)
                };
                self.set(*dst, r);
                false
            }
            IrOp::Mul {
                lo,
                hi,
                a,
                b,
                size,
                signed,
                set_flags,
            } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_mul(*lo, *hi, a, b, *size, *signed, *set_flags);
                false
            }
            IrOp::Div {
                quot,
                rem,
                hi,
                lo,
                divisor,
                size,
                signed,
            } => {
                let (hi, lo, dv) = (self.val(*hi), self.val(*lo), self.val(*divisor));
                self.emit_div(*quot, *rem, hi, lo, dv, *size, *signed);
                false
            }

            IrOp::GetCond { dst, cond } => {
                let c = self.eval_cond(*cond);
                let v = self.builder.ins().uextend(types::I64, c);
                self.set(*dst, v);
                false
            }

            IrOp::Load { dst, addr, size } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, *size, 0);
                let v = self.load_guest(host, *size);
                self.set(*dst, v);
                false
            }
            IrOp::Store {
                addr, src, size, ..
            } => {
                let a = self.val(*addr);
                let v = self.val(*src);
                let host = self.checked_addr(a, *size, 1);
                self.store_guest(host, v, *size);
                false
            }
            IrOp::AtomicRmw {
                old,
                addr,
                src,
                size,
                op,
            } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, *size, 1);
                let s = self.val(*src);
                let s = self.narrow(s, *size);
                let ty = int_ty(*size);
                let prev = if matches!(op, RmwOp::Rsub) {
                    // No native reverse-subtract atomic (`lock neg`): CAS loop with
                    // `new = s - cur`, retrying until the compare-exchange sticks.
                    let cur0 = self
                        .builder
                        .ins()
                        .atomic_load(ty, MemFlags::trusted(), host);
                    let loop_hdr = self.builder.create_block();
                    let cur = self.builder.append_block_param(loop_hdr, ty);
                    self.builder.ins().jump(loop_hdr, &[cur0]);
                    self.builder.switch_to_block(loop_hdr);
                    let new = self.builder.ins().isub(s, cur);
                    let seen = self
                        .builder
                        .ins()
                        .atomic_cas(MemFlags::trusted(), host, cur, new);
                    let ok = self.builder.ins().icmp(IntCC::Equal, seen, cur);
                    let done = self.builder.create_block();
                    // Retry (back-edge) on a lost race; `seen` is the old value on success.
                    self.builder.ins().brif(ok, done, &[], loop_hdr, &[seen]);
                    self.builder.seal_block(loop_hdr);
                    self.builder.switch_to_block(done);
                    self.builder.seal_block(done);
                    seen
                } else {
                    let cl_op = rmw_op(*op);
                    self.builder
                        .ins()
                        .atomic_rmw(ty, MemFlags::trusted(), cl_op, host, s)
                };
                let prev = self.widen(prev, *size);
                self.set(*old, prev);
                false
            }
            IrOp::AtomicCas {
                old,
                addr,
                expected,
                src,
                size,
            } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, *size, 1);
                let exp = self.val(*expected);
                let exp = self.narrow(exp, *size);
                let new = self.val(*src);
                let new = self.narrow(new, *size);
                let prev = self
                    .builder
                    .ins()
                    .atomic_cas(MemFlags::trusted(), host, exp, new);
                let prev = self.widen(prev, *size);
                self.set(*old, prev);
                false
            }
            IrOp::Bt {
                result,
                a,
                bit,
                size,
                op,
            } => {
                let av = self.val(*a);
                let bits = (*size as i64) * 8;
                let b = self.val(*bit);
                let b = self.builder.ins().band_imm(b, bits - 1);
                // CF = (a >> b) & 1
                let sh = self.builder.ins().ushr(av, b);
                let cf = self.builder.ins().band_imm(sh, 1);
                let cf = self.builder.ins().ireduce(types::I8, cf);
                self.store_flag(self.offsets.cf, cf);
                // result = a with the bit set / cleared / toggled
                let one = self.iconst(1);
                let mask = self.builder.ins().ishl(one, b);
                let r = match op {
                    BtOp::Test => av,
                    BtOp::Set => self.builder.ins().bor(av, mask),
                    BtOp::Reset => {
                        let nm = self.builder.ins().bnot(mask);
                        self.builder.ins().band(av, nm)
                    }
                    BtOp::Complement => self.builder.ins().bxor(av, mask),
                };
                self.set(*result, r);
                false
            }
            IrOp::Cpuid => {
                self.flush_gprs(); // helper reads RAX/RCX from CpuState
                let cpu = self.cpu;
                self.call_helper(self.helpers.cpuid, &[cpu]);
                self.reload_gprs(); // helper wrote RAX/RBX/RCX/RDX
                false
            }
            IrOp::X87 { kind, addr, sti } => {
                let a = self.val(*addr);
                let kc = self.iconst(*kind as u16 as u64);
                let stic = self.iconst(*sti as u64);
                let cur = self.iconst(self.cur_addr);
                let args = [self.cpu, self.mem, kc, a, stic, cur];
                self.flush_gprs(); // helper reads/writes CpuState
                let inst = self.call_helper(self.helpers.x87, &args);
                let code = self.builder.inst_results(inst)[0];
                let trapped = self
                    .builder
                    .ins()
                    .icmp_imm(IntCC::Equal, code, RET_UNMAPPED as i64);
                let exc = self.builder.create_block();
                let ok = self.builder.create_block();
                self.builder.ins().brif(trapped, exc, &[], ok, &[]);
                self.builder.seal_block(exc);
                self.builder.seal_block(ok);
                self.builder.switch_to_block(exc);
                // Helper set RIP + fault fields and is authoritative — don't re-flush.
                self.ret_no_flush(RET_UNMAPPED);
                self.builder.switch_to_block(ok);
                self.reload_gprs(); // e.g. fnstsw wrote AX
                false
            }
            IrOp::FxState { addr, restore } => {
                let a = self.val(*addr);
                let rc = self.iconst(*restore as u64);
                let cur = self.iconst(self.cur_addr);
                let args = [self.cpu, self.mem, a, rc, cur];
                self.flush_gprs(); // helper reads CpuState (XMM/x87)
                let inst = self.call_helper(self.helpers.fxstate, &args);
                let code = self.builder.inst_results(inst)[0];
                let trapped = self
                    .builder
                    .ins()
                    .icmp_imm(IntCC::Equal, code, RET_UNMAPPED as i64);
                let exc = self.builder.create_block();
                let ok = self.builder.create_block();
                self.builder.ins().brif(trapped, exc, &[], ok, &[]);
                self.builder.seal_block(exc);
                self.builder.seal_block(ok);
                self.builder.switch_to_block(exc);
                self.ret_no_flush(RET_UNMAPPED);
                self.builder.switch_to_block(ok);
                false
            }
            IrOp::Popcnt { dst, src, size } => {
                let s = self.val(*src);
                let s = self.mask(s, *size);
                let cnt = self.builder.ins().popcnt(s);
                self.set(*dst, cnt);
                let zero = self.iconst(0);
                let z = self.builder.ins().icmp(IntCC::Equal, s, zero);
                self.store_flag(self.offsets.zf, z);
                let z8 = self.builder.ins().iconst(types::I8, 0);
                for off in [
                    self.offsets.cf,
                    self.offsets.of,
                    self.offsets.sf,
                    self.offsets.af,
                    self.offsets.pf,
                ] {
                    self.store_flag(off, z8);
                }
                false
            }
            IrOp::Crc32 {
                dst,
                crc,
                src,
                bytes,
            } => {
                let c = self.val(*crc);
                let s = self.val(*src);
                let n = self.iconst(*bytes as u64);
                let inst = self.call_helper(self.helpers.crc32, &[c, s, n]);
                let r = self.builder.inst_results(inst)[0];
                self.set(*dst, r);
                false
            }
            IrOp::BitScan {
                dst,
                src,
                old,
                size,
                reverse,
            } => {
                let s = self.val(*src);
                let s = self.mask(s, *size);
                let zero = self.iconst(0);
                let is_zero = self.builder.ins().icmp(IntCC::Equal, s, zero);
                self.store_flag(self.offsets.zf, is_zero); // icmp already yields I8
                let idx = if *reverse {
                    let clz = self.builder.ins().clz(s);
                    self.builder.ins().irsub_imm(clz, 63) // 63 - clz
                } else {
                    self.builder.ins().ctz(s)
                };
                let old = self.val(*old);
                let old = self.mask(old, *size);
                // src==0 -> keep old; else the index.
                let r = self.builder.ins().select(is_zero, old, idx);
                self.set(*dst, r);
                false
            }

            IrOp::VLoad { dst, addr, size } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, *size, 0);
                let v = match size {
                    16 => self.gload(types::I128, host, 0),
                    8 => {
                        let x = self.gload(types::I64, host, 0);
                        self.builder.ins().uextend(types::I128, x)
                    }
                    _ => {
                        let x = self.gload(types::I32, host, 0);
                        self.builder.ins().uextend(types::I128, x)
                    }
                };
                self.store_xmm(*dst, v);
                false
            }
            IrOp::VStore { addr, src, size } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, *size, 1);
                let v = self.load_xmm(*src);
                match size {
                    16 => {
                        self.gstore(v, host, 0);
                    }
                    8 => {
                        let x = self.builder.ins().ireduce(types::I64, v);
                        self.gstore(x, host, 0);
                    }
                    _ => {
                        let x = self.builder.ins().ireduce(types::I32, v);
                        self.gstore(x, host, 0);
                    }
                }
                false
            }
            IrOp::VMov { dst, src } => {
                let v = self.load_xmm(*src);
                self.store_xmm(*dst, v);
                false
            }
            IrOp::VFromGpr { dst, src, size } => {
                let v = self.val(*src);
                let vm = self.mask(v, *size);
                let x = self.builder.ins().uextend(types::I128, vm);
                self.store_xmm(*dst, x);
                false
            }
            IrOp::VToGpr { dst, src, size } => {
                let v = self.load_xmm(*src);
                let lo = self.builder.ins().ireduce(types::I64, v);
                let r = self.mask(lo, *size);
                self.set(*dst, r);
                false
            }
            IrOp::VLogic { dst, a, b, op } => {
                let (va, vb) = (self.load_xmm(*a), self.load_xmm(*b));
                let r = match op {
                    VLogicOp::Xor => self.builder.ins().bxor(va, vb),
                    VLogicOp::And => self.builder.ins().band(va, vb),
                    VLogicOp::Or => self.builder.ins().bor(va, vb),
                    VLogicOp::Andn => {
                        let na = self.builder.ins().bnot(va);
                        self.builder.ins().band(na, vb)
                    }
                };
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VPackedBin {
                dst,
                a,
                b,
                lane,
                op,
            } => {
                let vty = vec_ty(*lane);
                let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
                let va = self.bitcast_v(xa, vty);
                let vb = self.bitcast_v(xb, vty);
                let r = self.emit_packed_bin(va, vb, *op);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VPackedBinM {
                dst,
                addr,
                lane,
                op,
            } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, 16, 0);
                let memv = self.gload(types::I128, host, 0);
                let vty = vec_ty(*lane);
                let xd = self.load_xmm(*dst);
                let vd = self.bitcast_v(xd, vty);
                let vm = self.bitcast_v(memv, vty);
                let r = self.emit_packed_bin(vd, vm, *op);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VLogicM { dst, addr, op } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, 16, 0);
                let memv = self.gload(types::I128, host, 0);
                let vd = self.load_xmm(*dst);
                let r = match op {
                    VLogicOp::Xor => self.builder.ins().bxor(vd, memv),
                    VLogicOp::And => self.builder.ins().band(vd, memv),
                    VLogicOp::Or => self.builder.ins().bor(vd, memv),
                    VLogicOp::Andn => {
                        let n = self.builder.ins().bnot(vd);
                        self.builder.ins().band(n, memv)
                    }
                };
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VPackedShift {
                dst,
                a,
                imm,
                lane,
                right,
                arith,
            } => {
                let vty = vec_ty(*lane);
                let bits = *lane as u32 * 8;
                let over = *imm as u32 >= bits; // x86: count >= width is defined
                let xa = self.load_xmm(*a);
                let va = self.bitcast_v(xa, vty);
                let zero128 = {
                    let z = self.iconst(0);
                    self.builder.ins().uextend(types::I128, z)
                };
                let r = if !*right {
                    if over {
                        zero128 // whole 128-bit result is zero
                    } else {
                        let amt = self.builder.ins().iconst(types::I32, *imm as i64);
                        let v = self.builder.ins().ishl(va, amt);
                        self.bitcast_i128(v)
                    }
                } else if !*arith {
                    if over {
                        zero128
                    } else {
                        let amt = self.builder.ins().iconst(types::I32, *imm as i64);
                        let v = self.builder.ins().ushr(va, amt);
                        self.bitcast_i128(v)
                    }
                } else {
                    // arithmetic right: an over-shift smears the sign bit.
                    let n = if over { bits - 1 } else { *imm as u32 };
                    let amt = self.builder.ins().iconst(types::I32, n as i64);
                    let v = self.builder.ins().sshr(va, amt);
                    self.bitcast_i128(v)
                };
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VByteShift {
                dst,
                a,
                bytes,
                right,
            } => {
                let v = self.load_xmm(*a);
                let r = if *bytes >= 16 {
                    let z = self.builder.ins().iconst(types::I64, 0);
                    self.builder.ins().uextend(types::I128, z)
                } else if *right {
                    self.builder.ins().ushr_imm(v, *bytes as i64 * 8)
                } else {
                    self.builder.ins().ishl_imm(v, *bytes as i64 * 8)
                };
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VShuffle32 { dst, a, imm } => {
                let mut mask = [0u8; 16];
                for i in 0..4 {
                    let sel = ((imm >> (2 * i)) & 3) as usize;
                    for j in 0..4 {
                        mask[i * 4 + j] = (sel * 4 + j) as u8;
                    }
                }
                let x = self.load_xmm(*a);
                let va = self.bitcast_v(x, types::I8X16);
                let r = self.shuffle(va, va, mask);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VMoveHalf {
                dst,
                src,
                dst_high,
                src_high,
            } => {
                let (xs, xd) = (self.load_xmm(*src), self.load_xmm(*dst));
                let sv = self.bitcast_v(xs, types::I64X2);
                let s = self.builder.ins().extractlane(sv, *src_high as u8);
                let dv = self.bitcast_v(xd, types::I64X2);
                let r = self.builder.ins().insertlane(dv, s, *dst_high as u8);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VLoadHalf { dst, addr, high } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, 8, 0);
                let v = self.gload(types::I64, host, 0);
                let xd = self.load_xmm(*dst);
                let dv = self.bitcast_v(xd, types::I64X2);
                let r = self.builder.ins().insertlane(dv, v, *high as u8);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VStoreHalf { addr, src, high } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, 8, 1);
                let xs = self.load_xmm(*src);
                let sv = self.bitcast_v(xs, types::I64X2);
                let s = self.builder.ins().extractlane(sv, *high as u8);
                self.gstore(s, host, 0);
                false
            }
            IrOp::VExtractW { dst, src, index } => {
                let x = self.load_xmm(*src);
                let vec = self.bitcast_v(x, types::I16X8);
                let w = self.builder.ins().extractlane(vec, *index & 7);
                let r = self.builder.ins().uextend(types::I64, w);
                self.set(*dst, r);
                false
            }
            IrOp::VExtractLane {
                dst,
                src,
                index,
                size,
            } => {
                let x = self.load_xmm(*src);
                let (ty, lanes) = match size {
                    1 => (types::I8X16, 16),
                    2 => (types::I16X8, 8),
                    4 => (types::I32X4, 4),
                    _ => (types::I64X2, 2),
                };
                let vec = self.bitcast_v(x, ty);
                let lane = self.builder.ins().extractlane(vec, *index % lanes);
                let r = if *size == 8 {
                    lane
                } else {
                    self.builder.ins().uextend(types::I64, lane)
                };
                self.set(*dst, r);
                false
            }
            IrOp::VMoveMaskB { dst, src } => {
                let x = self.load_xmm(*src);
                let v = self.bitcast_v(x, types::I8X16);
                let mask = self.builder.ins().vhigh_bits(types::I32, v);
                let r = self.builder.ins().uextend(types::I64, mask);
                self.set(*dst, r);
                false
            }
            IrOp::VZeroUpper { reg } => {
                self.store_ymm_hi_zero(*reg);
                false
            }
            IrOp::VZeroUpperAll => {
                for r in 0..16u8 {
                    self.store_ymm_hi_zero(r);
                }
                false
            }
            IrOp::VPshufb { dst, idx } => {
                let (xd, xi) = (self.load_xmm(*dst), self.load_xmm(*idx));
                let r = self.emit_pshufb(xd, xi);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VPshufbM { dst, addr } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, 16, 0);
                let iv = self.gload(types::I128, host, 0);
                let xd = self.load_xmm(*dst);
                let r = self.emit_pshufb(xd, iv);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VAlignr { dst, src, imm } => {
                let (xd, xs) = (self.load_xmm(*dst), self.load_xmm(*src));
                let r = self.emit_palignr(xd, xs, *imm);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VAlignrM { dst, addr, imm } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, 16, 0);
                let sv = self.gload(types::I128, host, 0);
                let xd = self.load_xmm(*dst);
                let r = self.emit_palignr(xd, sv, *imm);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VShufps { dst, a, b, imm } => {
                let mut mask = [0u8; 16];
                for i in 0..4 {
                    let sel = ((imm >> (2 * i)) & 3) as usize;
                    let base = if i < 2 { sel * 4 } else { 16 + sel * 4 };
                    for j in 0..4 {
                        mask[i * 4 + j] = (base + j) as u8;
                    }
                }
                let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
                let va = self.bitcast_v(xa, types::I8X16);
                let vb = self.bitcast_v(xb, types::I8X16);
                let r = self.shuffle(va, vb, mask);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VShuffle16 { dst, a, imm, high } => {
                let mut mask = [0u8; 16];
                for (b, m) in mask.iter_mut().enumerate() {
                    *m = b as u8; // identity for the untouched half
                }
                let base: usize = if *high { 8 } else { 0 };
                for i in 0..4 {
                    let sel = ((imm >> (2 * i)) & 3) as usize;
                    mask[base + i * 2] = (base + sel * 2) as u8;
                    mask[base + i * 2 + 1] = (base + sel * 2 + 1) as u8;
                }
                let x = self.load_xmm(*a);
                let va = self.bitcast_v(x, types::I8X16);
                let r = self.shuffle(va, va, mask);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VUnpackLow {
                dst,
                a,
                b,
                lane,
                high,
            } => {
                let mask = unpack_low_mask(*lane, *high);
                let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
                let va = self.bitcast_v(xa, types::I8X16);
                let vb = self.bitcast_v(xb, types::I8X16);
                let r = self.shuffle(va, vb, mask);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VPackUsWB { dst, a, b } => {
                let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
                let va = self.bitcast_v(xa, types::I16X8);
                let vb = self.bitcast_v(xb, types::I16X8);
                let c255 = self.builder.ins().iconst(types::I16, 255);
                let hi = self.builder.ins().splat(types::I16X8, c255);
                let c0 = self.builder.ins().iconst(types::I16, 0);
                let lo = self.builder.ins().splat(types::I16X8, c0);
                // Clamp each i16 lane to [0,255], then take the low byte of each
                // (uunarrow isn't lowered on x64, but the clamped value fits a byte).
                let ac = {
                    let m = self.builder.ins().smin(va, hi);
                    self.builder.ins().smax(m, lo)
                };
                let bc = {
                    let m = self.builder.ins().smin(vb, hi);
                    self.builder.ins().smax(m, lo)
                };
                let (aci, bci) = (self.bitcast_i128(ac), self.bitcast_i128(bc));
                let ab = self.bitcast_v(aci, types::I8X16);
                let bb = self.bitcast_v(bci, types::I8X16);
                let mask = [0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30];
                let packed = self.shuffle(ab, bb, mask);
                let r = self.bitcast_i128(packed);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VInsertW { dst, src, index } => {
                let x = self.load_xmm(*dst);
                let vec = self.bitcast_v(x, types::I16X8);
                let val = self.val(*src);
                let v16 = self.builder.ins().ireduce(types::I16, val);
                let r = self.builder.ins().insertlane(vec, v16, *index & 7);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }

            IrOp::VFloatMov { dst, src, prec } => {
                // Merge the low lane preserving the upper bytes (integer lane insert).
                let lty = lane_int_vec_ty(*prec);
                let (xd, xs) = (self.load_xmm(*dst), self.load_xmm(*src));
                let dv = self.bitcast_v(xd, lty);
                let sv = self.bitcast_v(xs, lty);
                let s0 = self.builder.ins().extractlane(sv, 0);
                let r = self.builder.ins().insertlane(dv, s0, 0);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VFloatBin {
                dst,
                a,
                b,
                op,
                prec,
                scalar,
            } => {
                let fty = float_vec_ty(*prec);
                let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
                let va = self.bitcast_v(xa, fty);
                let vb = self.bitcast_v(xb, fty);
                let r = if *scalar {
                    let x = self.builder.ins().extractlane(va, 0);
                    let y = self.builder.ins().extractlane(vb, 0);
                    let z = self.emit_fbin(x, y, *op);
                    self.builder.ins().insertlane(va, z, 0)
                } else {
                    self.emit_fbin(va, vb, *op)
                };
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VFloatBinM {
                dst,
                addr,
                op,
                prec,
                scalar,
            } => {
                let a = self.val(*addr);
                let fty = float_vec_ty(*prec);
                let xd = self.load_xmm(*dst);
                let vd = self.bitcast_v(xd, fty);
                let r = if *scalar {
                    let host = self.checked_addr(a, prec.bytes(), 0);
                    let y = self.gload(scalar_fty(*prec), host, 0);
                    let x = self.builder.ins().extractlane(vd, 0);
                    let z = self.emit_fbin(x, y, *op);
                    self.builder.ins().insertlane(vd, z, 0)
                } else {
                    let host = self.checked_addr(a, 16, 0);
                    let memv = self.gload(types::I128, host, 0);
                    let vb = self.bitcast_v(memv, fty);
                    self.emit_fbin(vd, vb, *op)
                };
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VFloatCmp { a, b, prec } => {
                let (av, bv) = (self.val(*a), self.val(*b));
                let (x, y) = match prec {
                    FPrec::F64 => (
                        self.bitcast_scalar(types::F64, av),
                        self.bitcast_scalar(types::F64, bv),
                    ),
                    FPrec::F32 => {
                        let (ai, bi) = (
                            self.builder.ins().ireduce(types::I32, av),
                            self.builder.ins().ireduce(types::I32, bv),
                        );
                        (
                            self.bitcast_scalar(types::F32, ai),
                            self.bitcast_scalar(types::F32, bi),
                        )
                    }
                };
                let un = self.builder.ins().fcmp(FloatCC::Unordered, x, y);
                let lt = self.builder.ins().fcmp(FloatCC::LessThan, x, y);
                let eq = self.builder.ins().fcmp(FloatCC::Equal, x, y);
                let zf = self.builder.ins().bor(un, eq);
                let cf = self.builder.ins().bor(un, lt);
                let zero = self.builder.ins().iconst(types::I8, 0);
                self.store_flag(self.offsets.cf, cf);
                self.store_flag(self.offsets.pf, un);
                self.store_flag(self.offsets.zf, zf);
                self.store_flag(self.offsets.af, zero);
                self.store_flag(self.offsets.sf, zero);
                self.store_flag(self.offsets.of, zero);
                false
            }
            IrOp::VFloatCmpMask {
                dst,
                a,
                b,
                prec,
                scalar,
                pred,
            } => {
                let fty = float_vec_ty(*prec);
                let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
                let va = self.bitcast_v(xa, fty);
                let vb = self.bitcast_v(xb, fty);
                // Build the per-lane mask (all-ones/0) from only the FloatCC
                // variants every host lowers — Equal/LessThan/LessThanOrEqual. The
                // "N"/UNORD/ORD forms are derived by bit-negation and self-compares
                // (ordered ⇔ a==a && b==b), matching `float_pred` in the core.
                // AArch64's vector fcmp can't lower the UnorderedOr*/OrderedNotEqual
                // predicates, so we never hand it one.
                let mask = match pred & 7 {
                    0 => self.builder.ins().fcmp(FloatCC::Equal, va, vb),
                    1 => self.builder.ins().fcmp(FloatCC::LessThan, va, vb),
                    2 => self.builder.ins().fcmp(FloatCC::LessThanOrEqual, va, vb),
                    3 => {
                        let ao = self.builder.ins().fcmp(FloatCC::Equal, va, va);
                        let bo = self.builder.ins().fcmp(FloatCC::Equal, vb, vb);
                        let ord = self.builder.ins().band(ao, bo);
                        self.builder.ins().bnot(ord)
                    }
                    4 => {
                        let eq = self.builder.ins().fcmp(FloatCC::Equal, va, vb);
                        self.builder.ins().bnot(eq)
                    }
                    5 => {
                        let lt = self.builder.ins().fcmp(FloatCC::LessThan, va, vb);
                        self.builder.ins().bnot(lt)
                    }
                    6 => {
                        let le = self.builder.ins().fcmp(FloatCC::LessThanOrEqual, va, vb);
                        self.builder.ins().bnot(le)
                    }
                    _ => {
                        let ao = self.builder.ins().fcmp(FloatCC::Equal, va, va);
                        let bo = self.builder.ins().fcmp(FloatCC::Equal, vb, vb);
                        self.builder.ins().band(ao, bo)
                    }
                };
                let ity = lane_int_vec_ty(*prec);
                let r = if *scalar {
                    let mi = self.bitcast_v(mask, ity);
                    let m0 = self.builder.ins().extractlane(mi, 0);
                    let xd = self.load_xmm(*dst);
                    let dv = self.bitcast_v(xd, ity);
                    let merged = self.builder.ins().insertlane(dv, m0, 0);
                    self.bitcast_i128(merged)
                } else {
                    let mi = self.bitcast_v(mask, ity);
                    self.bitcast_i128(mi)
                };
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VCvtFromInt {
                dst,
                src,
                int_size,
                prec,
            } => {
                let raw = self.val(*src);
                let signed = self.sign_extend(raw, *int_size);
                let f = self.builder.ins().fcvt_from_sint(scalar_fty(*prec), signed);
                let fbits = self.bitcast_scalar(lane_int_ty(*prec), f);
                let xd = self.load_xmm(*dst);
                let dv = self.bitcast_v(xd, lane_int_vec_ty(*prec));
                let r = self.builder.ins().insertlane(dv, fbits, 0);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VCvtToInt {
                dst,
                src,
                int_size,
                prec,
                trunc,
            } => {
                let raw = self.val(*src);
                let f = match prec {
                    FPrec::F64 => self.bitcast_scalar(types::F64, raw),
                    FPrec::F32 => {
                        let i = self.builder.ins().ireduce(types::I32, raw);
                        self.bitcast_scalar(types::F32, i)
                    }
                };
                // Round to nearest even for cvt*2si; cvtt*2si truncates toward zero.
                let f = if *trunc {
                    f
                } else {
                    self.builder.ins().nearest(f)
                };
                // Saturating convert matches the interpreter's Rust `as` cast (both
                // clamp out-of-range to the destination's INT_MIN/MAX; the x86
                // integer-indefinite result on invalid operands is deferred).
                let ity = if *int_size == 8 {
                    types::I64
                } else {
                    types::I32
                };
                let iv = self.builder.ins().fcvt_to_sint_sat(ity, f);
                let iv64 = if *int_size == 8 {
                    iv
                } else {
                    self.builder.ins().uextend(types::I64, iv)
                };
                self.set(*dst, iv64);
                false
            }
            IrOp::VCvtFloat { dst, src, from, to } => {
                let raw = self.val(*src);
                let f = match from {
                    FPrec::F64 => self.bitcast_scalar(types::F64, raw),
                    FPrec::F32 => {
                        let i = self.builder.ins().ireduce(types::I32, raw);
                        self.bitcast_scalar(types::F32, i)
                    }
                };
                let g = match to {
                    FPrec::F64 => self.builder.ins().fpromote(types::F64, f),
                    FPrec::F32 => self.builder.ins().fdemote(types::F32, f),
                };
                let gbits = self.bitcast_scalar(lane_int_ty(*to), g);
                let xd = self.load_xmm(*dst);
                let dv = self.bitcast_v(xd, lane_int_vec_ty(*to));
                let r = self.builder.ins().insertlane(dv, gbits, 0);
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VFloatUnary {
                dst,
                src,
                op,
                prec,
                scalar,
            } => {
                let fty = float_vec_ty(*prec);
                let xs = self.load_xmm(*src);
                let vs = self.bitcast_v(xs, fty);
                let r = if *scalar {
                    let s0 = self.builder.ins().extractlane(vs, 0);
                    let z = self.emit_funary(s0, *op);
                    // Preserve dst's upper lane(s).
                    let xd = self.load_xmm(*dst);
                    let vd = self.bitcast_v(xd, fty);
                    self.builder.ins().insertlane(vd, z, 0)
                } else {
                    self.emit_funary(vs, *op)
                };
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }

            IrOp::SetDf { value } => {
                let v = self.builder.ins().iconst(types::I8, *value as i64);
                self.store_flag(self.offsets.df, v);
                false
            }
            IrOp::RepString { op, elem, rep } => {
                let op_code = self.iconst(str_op_code(*op));
                let elem = self.iconst(*elem as u64);
                let rep = self.iconst(rep_code(*rep));
                let cur = self.iconst(self.cur_addr);
                let args = [self.cpu, self.mem, op_code, elem, rep, cur];
                self.flush_gprs(); // helper reads/advances RSI/RDI/RCX in CpuState
                let inst = self.call_helper(self.helpers.string, &args);
                let code = self.builder.inst_results(inst)[0];
                // code == RET_UNMAPPED (3) -> trap out; else continue.
                let trapped = self
                    .builder
                    .ins()
                    .icmp_imm(IntCC::Equal, code, RET_UNMAPPED as i64);
                let exc = self.builder.create_block();
                let ok = self.builder.create_block();
                self.builder.ins().brif(trapped, exc, &[], ok, &[]);
                self.builder.seal_block(exc);
                self.builder.seal_block(ok);
                self.builder.switch_to_block(exc);
                // Helper set RIP + fault fields and advanced RSI/RDI/RCX partway — it
                // is authoritative, so return without re-flushing stale Variables.
                self.ret_no_flush(RET_UNMAPPED);
                self.builder.switch_to_block(ok);
                self.reload_gprs(); // helper advanced RSI/RDI/RCX
                false
            }

            IrOp::Jump { target } => {
                let t = self.val(*target);
                self.store_cpu(self.offsets.rip, t);
                match target {
                    // Direct jump: known target, so chain through a link slot.
                    Val::Imm(_) => {
                        let slot = (self.alloc_slot)();
                        self.chain_or_link(slot);
                    }
                    // Indirect jump: target unknown at compile time. Probe the
                    // per-site IBTC (R4) — chain if the target repeats, else miss.
                    Val::Temp(_) => {
                        let slot = (self.alloc_slot)();
                        self.ibtc_or_miss(slot, t);
                    }
                }
                true
            }
            IrOp::Branch {
                cond,
                taken,
                fallthrough,
            } => {
                let c = self.eval_cond(*cond);
                let tk = self.builder.create_block();
                let fl = self.builder.create_block();
                self.builder.ins().brif(c, tk, &[], fl, &[]);
                self.builder.seal_block(tk);
                self.builder.seal_block(fl);
                self.builder.switch_to_block(tk);
                let ta = self.iconst(*taken);
                self.store_cpu(self.offsets.rip, ta);
                let tslot = (self.alloc_slot)();
                self.chain_or_link(tslot);
                self.builder.switch_to_block(fl);
                let fa = self.iconst(*fallthrough);
                self.store_cpu(self.offsets.rip, fa);
                let fslot = (self.alloc_slot)();
                self.chain_or_link(fslot);
                true
            }
            IrOp::Call {
                target,
                return_addr,
            } => {
                let rsp = self.read_gpr(RSP);
                let eight = self.iconst(8);
                let newsp = self.builder.ins().isub(rsp, eight);
                let host = self.checked_addr(newsp, 8, 1);
                let ra = self.iconst(*return_addr);
                self.store_guest(host, ra, 8);
                self.write_gpr(RSP, newsp, 8);
                let tgt = self.val(*target);
                self.store_cpu(self.offsets.rip, tgt);
                // Return prediction (R5): push (return_addr, continuation slot) onto
                // the shadow ring before transferring to the callee. The slot is an
                // ordinary link slot for the block at `return_addr`; the matching
                // `ret` chains through it. Done for both direct and indirect calls.
                let cont_slot = (self.alloc_slot)();
                self.emit_ret_push(*return_addr, cont_slot);
                match target {
                    // Direct call: the callee entry is known, so chain to it the
                    // same way a direct jump does (R2). The return-address push
                    // above already happened; only the transfer to the callee is
                    // chained. Indirect calls (Val::Temp) stay on the dispatch path
                    // until IBTC (R4).
                    Val::Imm(_) => {
                        let slot = (self.alloc_slot)();
                        self.chain_or_link(slot);
                    }
                    // Indirect call: IBTC-probe the computed callee (R4), same as an
                    // indirect jump. The return-address push above is unchanged.
                    Val::Temp(_) => {
                        let slot = (self.alloc_slot)();
                        self.ibtc_or_miss(slot, tgt);
                    }
                }
                true
            }
            IrOp::Ret => {
                let rsp = self.read_gpr(RSP);
                let host = self.checked_addr(rsp, 8, 0);
                let ret = self.load_guest(host, 8);
                let eight = self.iconst(8);
                let newsp = self.builder.ins().iadd(rsp, eight);
                self.write_gpr(RSP, newsp, 8);
                self.store_cpu(self.offsets.rip, ret);
                // Return prediction (R5): pop the shadow ring and chain to the
                // caller's continuation if the predicted address matches the actual
                // popped target; otherwise fall back to dispatch.
                self.emit_ret_predict(ret);
                true
            }
            IrOp::Syscall => {
                let end = self.iconst(self.guest_end);
                self.store_cpu(self.offsets.rip, end);
                self.ret(RET_SYSCALL);
                true
            }
            IrOp::Hlt => {
                let end = self.iconst(self.guest_end);
                self.store_cpu(self.offsets.rip, end);
                self.ret(RET_HLT);
                true
            }
        }
    }

    // --- ALU + flags (mirrors interp::alu_add / alu_sub / alu_logic) ---

    #[allow(clippy::too_many_arguments)]
    fn add_sub(
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
    fn emit_shift(
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
    fn emit_rcx(&mut self, dst: u32, a: Value, b: Value, size: u8, mask: FlagMask, left: bool) {
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
    fn emit_double_shift(
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
    fn emit_mul(
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
    fn emit_div(
        &mut self,
        quot_t: u32,
        rem_t: u32,
        hi: Value,
        lo: Value,
        divisor: Value,
        size: u8,
        signed: bool,
    ) {
        let ss = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            16,
            3,
        ));
        let out = self.builder.ins().stack_addr(types::I64, ss, 0);
        let sz = self.iconst(size as u64);
        let sg = self.iconst(signed as u64);
        let inst = self.call_helper(self.helpers.div, &[hi, lo, divisor, sz, sg, out]);
        let de = self.builder.inst_results(inst)[0];

        let exc = self.builder.create_block();
        let ok = self.builder.create_block();
        self.builder.ins().brif(de, exc, &[], ok, &[]);
        self.builder.seal_block(exc);
        self.builder.seal_block(ok);

        self.builder.switch_to_block(exc);
        let rip = self.iconst(self.cur_addr);
        self.store_cpu(self.offsets.rip, rip);
        self.ret(RET_EXCEPTION);

        self.builder.switch_to_block(ok);
        let q = self.builder.ins().stack_load(types::I64, ss, 0);
        let r = self.builder.ins().stack_load(types::I64, ss, 8);
        self.set(quot_t, q);
        self.set(rem_t, r);
    }

    fn mask_imm(&self, size: u8) -> i64 {
        if size >= 8 {
            -1
        } else {
            (1i64 << (size * 8)) - 1
        }
    }

    /// Rotate `vm` (masked to `size`) by `cnt`, within the operand width.
    fn rotate(&mut self, vm: Value, cnt: Value, size: u8, left: bool) -> Value {
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

    fn logic(&mut self, dst: u32, r: Value, size: u8, mask: FlagMask) {
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

    fn parity(&mut self, res: Value) -> Value {
        let low = self.builder.ins().band_imm(res, 0xff);
        let pc = self.builder.ins().popcnt(low);
        let lsb = self.builder.ins().band_imm(pc, 1);
        // Even parity → PF set.
        self.builder.ins().icmp_imm(IntCC::Equal, lsb, 0)
    }

    #[allow(clippy::too_many_arguments)]
    fn store_flags(
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

    fn eval_cond(&mut self, cond: Cond) -> Value {
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

    fn not(&mut self, b: Value) -> Value {
        self.builder.ins().bxor_imm(b, 1)
    }

    // --- memory ---

    /// Bounds-check `[addr, addr+size)` against the guest buffer; on failure store
    /// the fault info + RIP and return `RET_UNMAPPED`. Leaves the builder in the
    /// success block and returns the host address `base + addr`.
    fn checked_addr(&mut self, addr: Value, size: u8, access: u64) -> Value {
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
        let oob = self.builder.ins().bor(gt, ov);

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
        let host = self.builder.ins().iadd(base, addr);
        self.checked_ea.push((addr, size, host));
        host
    }

    /// Ordinary guest-RAM load at `host` (a host pointer into guest memory),
    /// applying the consistency tier's ordering (§8.2.3). On x86 (native TSO)
    /// this is a plain load in every tier; on ARM, `AcqRel`/`FullTso` add an
    /// acquire fence after the load (blocks Load→Load / Load→Store reordering).
    fn gload(&mut self, ty: Type, host: Value, off: i32) -> Value {
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
    fn gstore(&mut self, val: Value, host: Value, off: i32) {
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

    fn load_guest(&mut self, host: Value, size: u8) -> Value {
        let ty = int_ty(size);
        let v = self.gload(ty, host, 0);
        if size < 8 {
            self.builder.ins().uextend(types::I64, v)
        } else {
            v
        }
    }

    fn store_guest(&mut self, host: Value, val: Value, size: u8) {
        let v = if size < 8 {
            self.builder.ins().ireduce(int_ty(size), val)
        } else {
            val
        };
        self.gstore(v, host, 0);
    }

    // --- registers ---

    fn read_reg(&mut self, reg: Reg) -> Value {
        match reg.gpr_index() {
            Some(i) => self.read_gpr(i),
            None => self.load_cpu(self.reg_off(reg)),
        }
    }

    fn read_gpr(&mut self, index: usize) -> Value {
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

    fn write_reg(&mut self, reg: Reg, val: Value, size: u8) {
        match reg.gpr_index() {
            Some(i) => self.write_gpr(i, val, size),
            None => self.store_cpu(self.reg_off(reg), val),
        }
    }

    fn write_gpr(&mut self, index: usize, val: Value, size: u8) {
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
    fn flush_gprs(&mut self) {
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
    fn reload_gprs(&mut self) {
        if let Some(vars) = self.gpr_vars {
            for (i, &var) in vars.iter().enumerate() {
                let v = self.load_cpu(self.offsets.gpr(i));
                self.builder.def_var(var, v);
            }
        } else {
            self.gpr_cache = [None; 16];
        }
    }

    fn reg_off(&self, reg: Reg) -> i32 {
        match reg {
            Reg::Rip => self.offsets.rip,
            Reg::FsBase => self.offsets.fs_base,
            Reg::GsBase => self.offsets.gs_base,
            _ => unreachable!("non-gpr reg expected"),
        }
    }

    // --- primitives ---

    fn val(&mut self, v: Val) -> Value {
        match v {
            Val::Temp(t) => self.temps[t as usize].expect("temp defined before use"),
            Val::Imm(i) => self.iconst(i),
        }
    }

    fn set(&mut self, dst: u32, v: Value) {
        self.temps[dst as usize] = Some(v);
    }

    fn iconst(&mut self, v: u64) -> Value {
        self.builder.ins().iconst(types::I64, v as i64)
    }

    /// Call an imported Rust helper indirectly through its baked absolute address,
    /// so the compiled block emits no relocation for the call (AOT prerequisite).
    fn call_helper(&mut self, helper: (ir::SigRef, u64), args: &[Value]) -> ir::Inst {
        let (sig, addr) = helper;
        let callee = self.iconst(addr);
        self.builder.ins().call_indirect(sig, callee, args)
    }

    fn mask(&mut self, v: Value, size: u8) -> Value {
        if size >= 8 {
            v
        } else {
            let m = (1i64 << (size * 8)) - 1;
            self.builder.ins().band_imm(v, m)
        }
    }

    fn sign_bit(&self, size: u8) -> i64 {
        1i64 << (size * 8 - 1)
    }

    fn sign_extend(&mut self, v: Value, from: u8) -> Value {
        if from >= 8 {
            return v;
        }
        let shift = (64 - from * 8) as i64;
        let up = self.builder.ins().ishl_imm(v, shift);
        self.builder.ins().sshr_imm(up, shift)
    }

    fn shift_count(&mut self, b: Value, size: u8) -> Value {
        let m = if size == 8 { 63 } else { 31 };
        self.builder.ins().band_imm(b, m)
    }

    fn load_cpu(&mut self, off: i32) -> Value {
        self.builder
            .ins()
            .load(types::I64, MemFlags::trusted(), self.cpu, off)
    }

    fn store_cpu(&mut self, off: i32, v: Value) {
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.cpu, off);
    }

    /// Reinterpret an I128 as a vector type (same bits). Cranelift requires an
    /// endianness for a lane-count-changing bitcast; the guest is little-endian.
    fn bitcast_v(&mut self, v: Value, ty: Type) -> Value {
        let flags = MemFlags::new().with_endianness(ir::Endianness::Little);
        self.builder.ins().bitcast(ty, flags, v)
    }

    fn bitcast_i128(&mut self, v: Value) -> Value {
        let flags = MemFlags::new().with_endianness(ir::Endianness::Little);
        self.builder.ins().bitcast(types::I128, flags, v)
    }

    /// Reinterpret a scalar of the same bit width (int<->float). No lane count
    /// changes, so no endianness specifier is needed.
    fn bitcast_scalar(&mut self, ty: Type, v: Value) -> Value {
        self.builder.ins().bitcast(ty, MemFlags::new(), v)
    }

    /// Reduce a 64-bit value to the `size`-byte integer type (no-op at size 8).
    fn narrow(&mut self, v: Value, size: u8) -> Value {
        if size >= 8 {
            v
        } else {
            self.builder.ins().ireduce(int_ty(size), v)
        }
    }

    /// Zero-extend a `size`-byte integer back to I64 (no-op at size 8).
    fn widen(&mut self, v: Value, size: u8) -> Value {
        if size >= 8 {
            v
        } else {
            self.builder.ins().uextend(types::I64, v)
        }
    }

    /// Emit `pshufb`: mask the index bytes to `0x8F` (keep the zero-select bit and
    /// the low nibble) so a set top bit maps to an out-of-range lane, then use
    /// Cranelift's `swizzle` (out-of-range → 0). `data`/`idx` are raw I128.
    fn emit_pshufb(&mut self, data: Value, idx: Value) -> Value {
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
    fn emit_palignr(&mut self, dst: Value, src: Value, imm: u8) -> Value {
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

    /// Emit a packed integer op on two same-typed vectors.
    fn emit_packed_bin(&mut self, a: Value, b: Value, op: PackedBinOp) -> Value {
        match op {
            PackedBinOp::Add => self.builder.ins().iadd(a, b),
            PackedBinOp::Sub => self.builder.ins().isub(a, b),
            PackedBinOp::CmpEq => self.builder.ins().icmp(IntCC::Equal, a, b),
            PackedBinOp::CmpGt => self.builder.ins().icmp(IntCC::SignedGreaterThan, a, b),
            PackedBinOp::MinU => self.builder.ins().umin(a, b),
            PackedBinOp::MaxU => self.builder.ins().umax(a, b),
            PackedBinOp::MinS => self.builder.ins().smin(a, b),
            PackedBinOp::MaxS => self.builder.ins().smax(a, b),
        }
    }

    /// Emit a scalar or vector float unary op.
    fn emit_funary(&mut self, x: Value, op: FloatUnOp) -> Value {
        match op {
            FloatUnOp::Sqrt => self.builder.ins().sqrt(x),
        }
    }

    /// Emit a scalar or vector float arithmetic op. x86 min/max return the *second*
    /// operand on a NaN or equality, so they lower to an explicit compare+select
    /// (`(a<b)?a:b` / `(a>b)?a:b`) that matches the interpreter bit-for-bit, rather
    /// than an IEEE `fmin`/`fmax` (which differ on NaN).
    fn emit_fbin(&mut self, a: Value, b: Value, op: FloatBinOp) -> Value {
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
    fn shuffle(&mut self, a: Value, b: Value, mask: [u8; 16]) -> Value {
        let imm = self
            .builder
            .func
            .dfg
            .immediates
            .push(ConstantData::from(mask.as_slice()));
        self.builder.ins().shuffle(a, b, imm)
    }

    fn load_xmm(&mut self, index: u8) -> Value {
        let off = self.offsets.xmm(index as usize);
        self.builder
            .ins()
            .load(types::I128, MemFlags::trusted(), self.cpu, off)
    }

    fn store_xmm(&mut self, index: u8, v: Value) {
        let off = self.offsets.xmm(index as usize);
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.cpu, off);
    }

    /// Zero the upper 128 bits of YMM `index` (task-168.2) via two 8-byte stores.
    fn store_ymm_hi_zero(&mut self, index: u8) {
        let off = self.offsets.ymm_hi(index as usize);
        let z = self.builder.ins().iconst(types::I64, 0);
        self.builder
            .ins()
            .store(MemFlags::trusted(), z, self.cpu, off);
        self.builder
            .ins()
            .store(MemFlags::trusted(), z, self.cpu, off + 8);
    }

    fn load_mem(&mut self, off: i32) -> Value {
        self.builder
            .ins()
            .load(types::I64, MemFlags::trusted(), self.mem, off)
    }

    fn store_mem(&mut self, off: i32, v: Value) {
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.mem, off);
    }

    fn load_flag(&mut self, off: i32) -> Value {
        self.builder
            .ins()
            .load(types::I8, MemFlags::trusted(), self.cpu, off)
    }

    fn load_flag_u64(&mut self, off: i32) -> Value {
        let b = self.load_flag(off);
        self.builder.ins().uextend(types::I64, b)
    }

    fn store_flag(&mut self, off: i32, v: Value) {
        self.builder
            .ins()
            .store(MemFlags::trusted(), v, self.cpu, off);
    }

    /// Return to the dispatcher, flushing region GPRs first so `CpuState` is current
    /// (a no-op in single-block mode). Every exit and trap flows through here (incl.
    /// `chain_or_link`), so this one flush covers them all.
    fn ret(&mut self, code: u64) {
        self.flush_gprs();
        self.ret_no_flush(code);
    }

    /// Return WITHOUT flushing — for a helper's own trap path, where the helper has
    /// already written the authoritative `CpuState` (e.g. a partial `rep movs`) and
    /// flushing stale Variables over it would corrupt guest state.
    fn ret_no_flush(&mut self, code: u64) {
        let v = self.iconst(code);
        self.builder.ins().return_(&[v]);
    }

    /// Terminate a direct edge: load the link slot; if filled, hand the next
    /// entry back for a chained transfer, else ask the dispatcher to fill it.
    /// RIP is already stored by the caller.
    fn chain_or_link(&mut self, slot_addr: u64) {
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
    fn ibtc_or_miss(&mut self, slot_addr: u64, target: Value) {
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
    fn ret_stack_ptr(&mut self) -> Value {
        self.builder
            .ins()
            .load(types::I64, MemFlags::trusted(), self.mem, MEMCTX_RET_STACK)
    }

    /// Byte address of ring frame `sp & (LEN-1)` given the ring base and a `sp`.
    fn ret_frame_addr(&mut self, rs: Value, sp: Value) -> Value {
        let idx = self.builder.ins().band_imm(sp, (RET_STACK_LEN - 1) as i64);
        let stride = self.builder.ins().imul_imm(idx, RETSTACK_STRIDE as i64);
        let off = self.builder.ins().iadd_imm(stride, RETSTACK_ENTRIES as i64);
        self.builder.ins().iadd(rs, off)
    }

    /// Push a predicted return frame `(return_addr, cont_slot_addr)` onto the shadow
    /// ring (R5). Wrap-and-overwrite on overflow — a lost frame only costs a later
    /// misprediction, never a wrong transfer.
    fn emit_ret_push(&mut self, return_addr: u64, cont_slot_addr: u64) {
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
    fn emit_ret_predict(&mut self, actual: Value) {
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
    fn emit_fuel_gate(&mut self, block_addr: u64) {
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
    fn emit_region_block(&mut self, block: &IrBlock, clif: &HashMap<u64, Block>) {
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
    fn region_edge(&mut self, target: u64, clif: &HashMap<u64, Block>) -> (Block, Option<u64>) {
        match clif.get(&target) {
            Some(&b) => (b, None),                               // internal edge
            None => (self.builder.create_block(), Some(target)), // exit stub, filled by the caller
        }
    }

    /// Fill an exit stub: store RIP and chain/link out to `target_addr`.
    fn fill_region_exit(&mut self, stub: Block, target_addr: u64) {
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
            x87: mk(),
            fxstate: mk(),
            crc32: mk(),
        };

        let mut slot = 0u64;
        let mut alloc = || {
            slot += 1;
            slot
        };
        translate_block(&mut builder, &ir, &offsets, &mut alloc, helpers, tier, None);
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
