//! Translate an `IrBlock` to Cranelift IR (§8.2.3). One `match` on `IrOp`, but
//! describing operations to a `FunctionBuilder` instead of executing them. Flag
//! computation mirrors the interpreter (`interp.rs`) exactly so the two backends
//! agree bit-for-bit (the M4 acceptance oracle).

use cranelift::prelude::*;

use x86jit_core::jit_abi::{
    CpuOffsets, MEMCTX_BASE, MEMCTX_FAULT_ACCESS, MEMCTX_FAULT_ADDR, MEMCTX_FAULT_SIZE,
    MEMCTX_LINK_SLOT, MEMCTX_NEXT_ENTRY, MEMCTX_SIZE, RET_CHAIN, RET_CONTINUE, RET_HLT, RET_LINK,
    RET_SYSCALL, RET_UNMAPPED,
};
use x86jit_core::{Cond, FlagMask, IrBlock, IrOp, Reg, Val};

const RSP: usize = 4;

/// `alloc_slot` hands out a stable heap address for a link slot (a `*const u8`
/// initialized to null); the block bakes it as a constant and the dispatcher
/// fills it when the edge is first taken (§12 M5).
pub fn translate_block(
    builder: &mut FunctionBuilder,
    ir: &IrBlock,
    offsets: &CpuOffsets,
    alloc_slot: &mut dyn FnMut() -> u64,
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

struct Translator<'a, 'b> {
    builder: &'a mut FunctionBuilder<'b>,
    offsets: &'a CpuOffsets,
    cpu: Value,
    mem: Value,
    temps: Vec<Option<Value>>,
    cur_addr: u64,
    guest_end: u64,
    alloc_slot: &'a mut dyn FnMut() -> u64,
}

impl Translator<'_, '_> {
    /// Translate one op; return `true` if it terminated the block.
    fn op(&mut self, op: &IrOp) -> bool {
        match op {
            IrOp::InsnStart { guest_addr } => {
                self.cur_addr = *guest_addr;
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

            IrOp::Add { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let zero = self.iconst(0);
                self.add_sub(*dst, a, b, zero, *size, *set_flags, false);
                false
            }
            IrOp::Adc { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let cin = self.load_flag_u64(self.offsets.cf);
                self.add_sub(*dst, a, b, cin, *size, *set_flags, false);
                false
            }
            IrOp::Sub { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let zero = self.iconst(0);
                self.add_sub(*dst, a, b, zero, *size, *set_flags, true);
                false
            }
            IrOp::Sbb { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let cin = self.load_flag_u64(self.offsets.cf);
                self.add_sub(*dst, a, b, cin, *size, *set_flags, true);
                false
            }
            IrOp::And { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let r = self.builder.ins().band(a, b);
                self.logic(*dst, r, *size, *set_flags);
                false
            }
            IrOp::Or { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let r = self.builder.ins().bor(a, b);
                self.logic(*dst, r, *size, *set_flags);
                false
            }
            IrOp::Xor { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let r = self.builder.ins().bxor(a, b);
                self.logic(*dst, r, *size, *set_flags);
                false
            }

            IrOp::Shl { dst, a, b, size, .. } => {
                let (a, b) = (self.val(*a), self.val(*b));
                let cnt = self.shift_count(b, *size);
                let sh = self.builder.ins().ishl(a, cnt);
                let r = self.mask(sh, *size);
                self.set(*dst, r);
                false
            }
            IrOp::Shr { dst, a, b, size, .. } => {
                let a = self.val(*a);
                let am = self.mask(a, *size);
                let b = self.val(*b);
                let cnt = self.shift_count(b, *size);
                let r = self.builder.ins().ushr(am, cnt);
                self.set(*dst, r);
                false
            }
            IrOp::Sar { dst, a, b, size, .. } => {
                let a = self.val(*a);
                let se = self.sign_extend(a, *size);
                let b = self.val(*b);
                let cnt = self.shift_count(b, *size);
                let sh = self.builder.ins().sshr(se, cnt);
                let r = self.mask(sh, *size);
                self.set(*dst, r);
                false
            }
            IrOp::Sext { dst, a, from } => {
                let a = self.val(*a);
                let r = self.sign_extend(a, *from);
                self.set(*dst, r);
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
            IrOp::Store { addr, src, size, .. } => {
                let a = self.val(*addr);
                let v = self.val(*src);
                let host = self.checked_addr(a, *size, 1);
                self.store_guest(host, v, *size);
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
                    // Indirect jump: target unknown at compile time — back to dispatch.
                    Val::Temp(_) => self.ret(RET_CONTINUE),
                }
                true
            }
            IrOp::Branch { cond, taken, fallthrough } => {
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
            IrOp::Call { target, return_addr } => {
                let rsp = self.read_gpr(RSP);
                let eight = self.iconst(8);
                let newsp = self.builder.ins().isub(rsp, eight);
                let host = self.checked_addr(newsp, 8, 1);
                let ra = self.iconst(*return_addr);
                self.store_guest(host, ra, 8);
                self.write_gpr(RSP, newsp, 8);
                let tgt = self.val(*target);
                self.store_cpu(self.offsets.rip, tgt);
                self.ret(RET_CONTINUE);
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
                self.ret(RET_CONTINUE);
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
    fn add_sub(&mut self, dst: u32, a: Value, b: Value, cin: Value, size: u8, mask: FlagMask, sub: bool) {
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
    fn store_flags(&mut self, mask: FlagMask, cf: Value, pf: Value, af: Value, zf: Value, sf: Value, of: Value) {
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
        let memsize = self.load_mem(MEMCTX_SIZE);
        let szc = self.iconst(size as u64);
        let end = self.builder.ins().iadd(addr, szc);
        let gt = self.builder.ins().icmp(IntCC::UnsignedGreaterThan, end, memsize);
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
        let base = self.load_mem(MEMCTX_BASE);
        self.builder.ins().iadd(base, addr)
    }

    fn load_guest(&mut self, host: Value, size: u8) -> Value {
        let ty = int_ty(size);
        let v = self.builder.ins().load(ty, MemFlags::trusted(), host, 0);
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
        self.builder.ins().store(MemFlags::trusted(), v, host, 0);
    }

    // --- registers ---

    fn read_reg(&mut self, reg: Reg) -> Value {
        match reg.gpr_index() {
            Some(i) => self.read_gpr(i),
            None => self.load_cpu(self.reg_off(reg)),
        }
    }

    fn read_gpr(&mut self, index: usize) -> Value {
        self.load_cpu(self.offsets.gpr(index))
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
                let cur = self.load_cpu(off);
                let hi = self.builder.ins().band_imm(cur, !0xffffi64);
                let lo = self.builder.ins().band_imm(val, 0xffff);
                self.builder.ins().bor(hi, lo)
            }
            1 => {
                let cur = self.load_cpu(off);
                let hi = self.builder.ins().band_imm(cur, !0xffi64);
                let lo = self.builder.ins().band_imm(val, 0xff);
                self.builder.ins().bor(hi, lo)
            }
            _ => unreachable!("gpr write size 1/2/4/8"),
        };
        self.store_cpu(off, new);
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
        self.builder.ins().load(types::I64, MemFlags::trusted(), self.cpu, off)
    }

    fn store_cpu(&mut self, off: i32, v: Value) {
        self.builder.ins().store(MemFlags::trusted(), v, self.cpu, off);
    }

    fn load_mem(&mut self, off: i32) -> Value {
        self.builder.ins().load(types::I64, MemFlags::trusted(), self.mem, off)
    }

    fn store_mem(&mut self, off: i32, v: Value) {
        self.builder.ins().store(MemFlags::trusted(), v, self.mem, off);
    }

    fn load_flag(&mut self, off: i32) -> Value {
        self.builder.ins().load(types::I8, MemFlags::trusted(), self.cpu, off)
    }

    fn load_flag_u64(&mut self, off: i32) -> Value {
        let b = self.load_flag(off);
        self.builder.ins().uextend(types::I64, b)
    }

    fn store_flag(&mut self, off: i32, v: Value) {
        self.builder.ins().store(MemFlags::trusted(), v, self.cpu, off);
    }

    fn ret(&mut self, code: u64) {
        let v = self.iconst(code);
        self.builder.ins().return_(&[v]);
    }

    /// Terminate a direct edge: load the link slot; if filled, hand the next
    /// entry back for a chained transfer, else ask the dispatcher to fill it.
    /// RIP is already stored by the caller.
    fn chain_or_link(&mut self, slot_addr: u64) {
        let slot = self.iconst(slot_addr);
        let entry = self.builder.ins().load(types::I64, MemFlags::trusted(), slot, 0);
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
