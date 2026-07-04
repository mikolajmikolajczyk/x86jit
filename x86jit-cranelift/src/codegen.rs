//! Translate an `IrBlock` to Cranelift IR (§8.2.3). One `match` on `IrOp`, but
//! describing operations to a `FunctionBuilder` instead of executing them. Flag
//! computation mirrors the interpreter (`interp.rs`) exactly so the two backends
//! agree bit-for-bit (the M4 acceptance oracle).

use cranelift::prelude::*;

use cranelift::codegen::ir::{self, ConstantData, FuncRef, StackSlotData, StackSlotKind};

use x86jit_core::jit_abi::{
    CpuOffsets, MEMCTX_BASE, MEMCTX_FAULT_ACCESS, MEMCTX_FAULT_ADDR, MEMCTX_FAULT_SIZE,
    MEMCTX_LINK_SLOT, MEMCTX_NEXT_ENTRY, MEMCTX_SIZE, RET_CHAIN, RET_CONTINUE, RET_EXCEPTION,
    RET_HLT, RET_LINK, RET_SYSCALL, RET_UNMAPPED,
};
use x86jit_core::{
    Cond, FPrec, FlagMask, FloatBinOp, IrBlock, IrOp, PackedBinOp, Reg, RepKind, RmwOp, StrOp, Val,
    VLogicOp,
};

const RSP: usize = 4;

/// `alloc_slot` hands out a stable heap address for a link slot (a `*const u8`
/// initialized to null); the block bakes it as a constant and the dispatcher
/// fills it when the edge is first taken (§12 M5). `div_ref` is the imported
/// division helper.
/// Imported Rust helpers callable from compiled blocks (§14, §10).
#[derive(Copy, Clone)]
pub struct Helpers {
    pub div: FuncRef,
    pub string: FuncRef,
}

pub fn translate_block(
    builder: &mut FunctionBuilder,
    ir: &IrBlock,
    offsets: &CpuOffsets,
    alloc_slot: &mut dyn FnMut() -> u64,
    helpers: Helpers,
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
    helpers: Helpers,
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

            IrOp::Shl { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Shl, a, b, *size, *set_flags);
                false
            }
            IrOp::Shr { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Shr, a, b, *size, *set_flags);
                false
            }
            IrOp::Sar { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Sar, a, b, *size, *set_flags);
                false
            }
            IrOp::Rol { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Rol, a, b, *size, *set_flags);
                false
            }
            IrOp::Ror { dst, a, b, size, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_shift(*dst, ShiftKind::Ror, a, b, *size, *set_flags);
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
            IrOp::Mul { lo, hi, a, b, size, signed, set_flags } => {
                let (a, b) = (self.val(*a), self.val(*b));
                self.emit_mul(*lo, *hi, a, b, *size, *signed, *set_flags);
                false
            }
            IrOp::Div { quot, rem, hi, lo, divisor, size, signed } => {
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
            IrOp::Store { addr, src, size, .. } => {
                let a = self.val(*addr);
                let v = self.val(*src);
                let host = self.checked_addr(a, *size, 1);
                self.store_guest(host, v, *size);
                false
            }
            IrOp::AtomicRmw { old, addr, src, size, op } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, *size, 1);
                let s = self.val(*src);
                let s = self.narrow(s, *size);
                let cl_op = rmw_op(*op);
                let prev = self.builder.ins().atomic_rmw(int_ty(*size), MemFlags::trusted(), cl_op, host, s);
                let prev = self.widen(prev, *size);
                self.set(*old, prev);
                false
            }
            IrOp::AtomicCas { old, addr, expected, src, size } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, *size, 1);
                let exp = self.val(*expected);
                let exp = self.narrow(exp, *size);
                let new = self.val(*src);
                let new = self.narrow(new, *size);
                let prev = self.builder.ins().atomic_cas(MemFlags::trusted(), host, exp, new);
                let prev = self.widen(prev, *size);
                self.set(*old, prev);
                false
            }

            IrOp::VLoad { dst, addr, size } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, *size, 0);
                let v = match size {
                    16 => self.builder.ins().load(types::I128, MemFlags::trusted(), host, 0),
                    8 => {
                        let x = self.builder.ins().load(types::I64, MemFlags::trusted(), host, 0);
                        self.builder.ins().uextend(types::I128, x)
                    }
                    _ => {
                        let x = self.builder.ins().load(types::I32, MemFlags::trusted(), host, 0);
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
                        self.builder.ins().store(MemFlags::trusted(), v, host, 0);
                    }
                    8 => {
                        let x = self.builder.ins().ireduce(types::I64, v);
                        self.builder.ins().store(MemFlags::trusted(), x, host, 0);
                    }
                    _ => {
                        let x = self.builder.ins().ireduce(types::I32, v);
                        self.builder.ins().store(MemFlags::trusted(), x, host, 0);
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
            IrOp::VPackedBin { dst, a, b, lane, op } => {
                let vty = vec_ty(*lane);
                let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
                let va = self.bitcast_v(xa, vty);
                let vb = self.bitcast_v(xb, vty);
                let r = match op {
                    PackedBinOp::Add => self.builder.ins().iadd(va, vb),
                    PackedBinOp::Sub => self.builder.ins().isub(va, vb),
                    PackedBinOp::CmpEq => self.builder.ins().icmp(IntCC::Equal, va, vb),
                };
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VPackedBinM { dst, addr, lane, op } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, 16, 0);
                let memv = self.builder.ins().load(types::I128, MemFlags::trusted(), host, 0);
                let vty = vec_ty(*lane);
                let xd = self.load_xmm(*dst);
                let vd = self.bitcast_v(xd, vty);
                let vm = self.bitcast_v(memv, vty);
                let r = match op {
                    PackedBinOp::Add => self.builder.ins().iadd(vd, vm),
                    PackedBinOp::Sub => self.builder.ins().isub(vd, vm),
                    PackedBinOp::CmpEq => self.builder.ins().icmp(IntCC::Equal, vd, vm),
                };
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VLogicM { dst, addr, op } => {
                let a = self.val(*addr);
                let host = self.checked_addr(a, 16, 0);
                let memv = self.builder.ins().load(types::I128, MemFlags::trusted(), host, 0);
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
            IrOp::VPackedShift { dst, a, imm, lane, right } => {
                let vty = vec_ty(*lane);
                let xa = self.load_xmm(*a);
                let va = self.bitcast_v(xa, vty);
                let amt = self.builder.ins().iconst(types::I32, *imm as i64);
                let r = if *right {
                    self.builder.ins().ushr(va, amt)
                } else {
                    self.builder.ins().ishl(va, amt)
                };
                let r = self.bitcast_i128(r);
                self.store_xmm(*dst, r);
                false
            }
            IrOp::VByteShiftR { dst, a, bytes } => {
                let v = self.load_xmm(*a);
                let r = if *bytes >= 16 {
                    let z = self.builder.ins().iconst(types::I64, 0);
                    self.builder.ins().uextend(types::I128, z)
                } else {
                    self.builder.ins().ushr_imm(v, *bytes as i64 * 8)
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
            IrOp::VUnpackLow { dst, a, b, lane } => {
                let mask = unpack_low_mask(*lane);
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
            IrOp::VFloatBin { dst, a, b, op, prec, scalar } => {
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
            IrOp::VFloatBinM { dst, addr, op, prec, scalar } => {
                let a = self.val(*addr);
                let fty = float_vec_ty(*prec);
                let xd = self.load_xmm(*dst);
                let vd = self.bitcast_v(xd, fty);
                let r = if *scalar {
                    let host = self.checked_addr(a, prec.bytes(), 0);
                    let y = self.builder.ins().load(scalar_fty(*prec), MemFlags::trusted(), host, 0);
                    let x = self.builder.ins().extractlane(vd, 0);
                    let z = self.emit_fbin(x, y, *op);
                    self.builder.ins().insertlane(vd, z, 0)
                } else {
                    let host = self.checked_addr(a, 16, 0);
                    let memv = self.builder.ins().load(types::I128, MemFlags::trusted(), host, 0);
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
                    FPrec::F64 => (self.bitcast_scalar(types::F64, av), self.bitcast_scalar(types::F64, bv)),
                    FPrec::F32 => {
                        let (ai, bi) = (
                            self.builder.ins().ireduce(types::I32, av),
                            self.builder.ins().ireduce(types::I32, bv),
                        );
                        (self.bitcast_scalar(types::F32, ai), self.bitcast_scalar(types::F32, bi))
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
            IrOp::VCvtFromInt { dst, src, int_size, prec } => {
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
            IrOp::VCvtToInt { dst, src, int_size, prec, trunc } => {
                let raw = self.val(*src);
                let f = match prec {
                    FPrec::F64 => self.bitcast_scalar(types::F64, raw),
                    FPrec::F32 => {
                        let i = self.builder.ins().ireduce(types::I32, raw);
                        self.bitcast_scalar(types::F32, i)
                    }
                };
                // Round to nearest even for cvt*2si; cvtt*2si truncates toward zero.
                let f = if *trunc { f } else { self.builder.ins().nearest(f) };
                // Saturating convert matches the interpreter's Rust `as` cast (both
                // clamp out-of-range to the destination's INT_MIN/MAX; the x86
                // integer-indefinite result on invalid operands is deferred).
                let ity = if *int_size == 8 { types::I64 } else { types::I32 };
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
                let inst = self.builder.ins().call(self.helpers.string, &args);
                let code = self.builder.inst_results(inst)[0];
                // code == RET_UNMAPPED (3) -> trap out; else continue.
                let trapped = self.builder.ins().icmp_imm(IntCC::Equal, code, RET_UNMAPPED as i64);
                let exc = self.builder.create_block();
                let ok = self.builder.create_block();
                self.builder.ins().brif(trapped, exc, &[], ok, &[]);
                self.builder.seal_block(exc);
                self.builder.seal_block(ok);
                self.builder.switch_to_block(exc);
                // The helper already set RIP + fault fields.
                self.ret(RET_UNMAPPED);
                self.builder.switch_to_block(ok);
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

    /// Shift with count-conditional flags (§16): compute the result always, but
    /// only update flags when the masked count != 0 — mirrors the interpreter.
    fn emit_shift(&mut self, dst: u32, kind: ShiftKind, a: Value, b: Value, size: u8, mask: FlagMask) {
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
                let le = self.builder.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, cnt, n);
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

    /// Widening multiply (mirrors interp `Mul`). CF=OF set iff the product spills
    /// the low half. For size ≤ 4 the product fits in I64; size 8 uses umulhi/smulhi.
    #[allow(clippy::too_many_arguments)]
    fn emit_mul(&mut self, lo_t: u32, hi_t: u32, a: Value, b: Value, size: u8, signed: bool, mask: FlagMask) {
        let m = self.mask_imm(size);
        let (lo, hi, overflow) = if size < 8 {
            let n = (size * 8) as i64;
            let (va, vb) = if signed {
                (self.sign_extend(a, size), self.sign_extend(b, size))
            } else {
                (self.builder.ins().band_imm(a, m), self.builder.ins().band_imm(b, m))
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
    fn emit_div(&mut self, quot_t: u32, rem_t: u32, hi: Value, lo: Value, divisor: Value, size: u8, signed: bool) {
        let ss = self
            .builder
            .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 16, 3));
        let out = self.builder.ins().stack_addr(types::I64, ss, 0);
        let sz = self.iconst(size as u64);
        let sg = self.iconst(signed as u64);
        let inst = self.builder.ins().call(self.helpers.div, &[hi, lo, divisor, sz, sg, out]);
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

    /// Emit a scalar or vector float arithmetic op.
    fn emit_fbin(&mut self, a: Value, b: Value, op: FloatBinOp) -> Value {
        match op {
            FloatBinOp::Add => self.builder.ins().fadd(a, b),
            FloatBinOp::Sub => self.builder.ins().fsub(a, b),
            FloatBinOp::Mul => self.builder.ins().fmul(a, b),
            FloatBinOp::Div => self.builder.ins().fdiv(a, b),
        }
    }

    /// Byte-permute shuffle of two I8X16 vectors by a compile-time mask (0–15
    /// select from `a`, 16–31 from `b`).
    fn shuffle(&mut self, a: Value, b: Value, mask: [u8; 16]) -> Value {
        let imm = self.builder.func.dfg.immediates.push(ConstantData::from(mask.as_slice()));
        self.builder.ins().shuffle(a, b, imm)
    }

    fn load_xmm(&mut self, index: u8) -> Value {
        let off = self.offsets.xmm(index as usize);
        self.builder.ins().load(types::I128, MemFlags::trusted(), self.cpu, off)
    }

    fn store_xmm(&mut self, index: u8, v: Value) {
        let off = self.offsets.xmm(index as usize);
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
fn unpack_low_mask(lane: u8) -> [u8; 16] {
    let mut mask = [0u8; 16];
    let n = 8 / lane; // elements from the low half
    let mut out = 0usize;
    for k in 0..n {
        for j in 0..lane {
            mask[out] = k * lane + j; // a element k, byte j
            out += 1;
        }
        for j in 0..lane {
            mask[out] = 16 + k * lane + j; // b element k
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
