use super::*;

impl Translator<'_, '_> {
    pub(crate) fn emit_v_load(&mut self, dst: &u8, addr: &Val, size: &u8) -> bool {
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

    pub(crate) fn emit_v_store(&mut self, addr: &Val, src: &u8, size: &u8) -> bool {
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

    pub(crate) fn emit_v_mov(&mut self, dst: &u8, src: &u8) -> bool {
        let v = self.load_xmm(*src);
        self.store_xmm(*dst, v);
        false
    }

    pub(crate) fn emit_v_load_wide(&mut self, dst: &u8, addr: &Val, bytes: &u16) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, *bytes as u8, 0);
        let n = *bytes as usize / 16;
        for i in 0..n {
            let v = self.gload(types::I128, host, (i * 16) as i32);
            self.store_lane(*dst, i, v);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_store_wide(&mut self, addr: &Val, src: &u8, bytes: &u16) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, *bytes as u8, 1);
        for i in 0..*bytes as usize / 16 {
            let v = self.load_lane(*src, i);
            self.gstore(v, host, (i * 16) as i32);
        }
        false
    }

    pub(crate) fn emit_v_mov_wide(&mut self, dst: &u8, src: &u8, bytes: &u16) -> bool {
        let n = *bytes as usize / 16;
        for i in 0..n {
            let v = self.load_lane(*src, i);
            self.store_lane(*dst, i, v);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_mask_mov(
        &mut self,
        dst: &u8,
        src: &u8,
        k: &u8,
        elem: &u8,
        zeroing: &bool,
        bytes: &u16,
    ) -> bool {
        // Delegate the per-element blend to the shared write_masked (decision-13):
        // masked ops aren't hot, and this guarantees jit == interp. The helper
        // writes the vector reg in CpuState directly (vector state is memory-backed,
        // so later load_xmm sees it); GPRs untouched, so no flush/reload.
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let s = self.iconst(*src as u64);
        let kk = self.iconst(*k as u64);
        let el = self.iconst(*elem as u64);
        let z = self.iconst(*zeroing as u64);
        let by = self.iconst(*bytes as u64);
        self.call_helper(self.helpers.vmaskmov, &[cpu, d, s, kk, el, z, by]);
        false
    }

    pub(crate) fn emit_v_mask_load_mem(
        &mut self,
        dst: &u8,
        addr: &Val,
        k: &u8,
        elem: &u8,
        zeroing: &bool,
        bytes: &u16,
    ) -> bool {
        // Masked memory move via the shared, fault-capable helper (decision-13):
        // element-wise so masked-off lanes never fault, guaranteeing jit == interp.
        // The helper writes the dst vector in CpuState (memory-backed); on an
        // active-lane fault it sets RIP + fault fields and returns RET_UNMAPPED.
        let cpu = self.cpu;
        let mem = self.mem;
        let base = self.val(*addr);
        let reg = self.iconst(*dst as u64);
        let kk = self.iconst(*k as u64);
        let el = self.iconst(*elem as u64);
        let z = self.iconst(*zeroing as u64);
        let by = self.iconst(*bytes as u64);
        let is_store = self.iconst(0);
        let cur = self.iconst(self.cur_addr);
        self.flush_gprs(); // helper's fault path reads the committed CpuState
        let inst = self.call_helper(
            self.helpers.vmaskmov_mem,
            &[cpu, mem, reg, base, kk, el, z, by, is_store, cur],
        );
        self.trap_if_unmapped(inst);
        false
    }

    pub(crate) fn emit_v_mask_store_mem(
        &mut self,
        src: &u8,
        addr: &Val,
        k: &u8,
        elem: &u8,
        bytes: &u16,
    ) -> bool {
        let cpu = self.cpu;
        let mem = self.mem;
        let base = self.val(*addr);
        let reg = self.iconst(*src as u64);
        let kk = self.iconst(*k as u64);
        let el = self.iconst(*elem as u64);
        let z = self.iconst(0);
        let by = self.iconst(*bytes as u64);
        let is_store = self.iconst(1);
        let cur = self.iconst(self.cur_addr);
        self.flush_gprs();
        let inst = self.call_helper(
            self.helpers.vmaskmov_mem,
            &[cpu, mem, reg, base, kk, el, z, by, is_store, cur],
        );
        self.trap_if_unmapped(inst);
        false
    }

    pub(crate) fn emit_v_insert_lane_wide(
        &mut self,
        dst: &u8,
        src: &u8,
        ins: &u8,
        idx: &u8,
        num_lanes: &u8,
        bytes: &u16,
    ) -> bool {
        let n = *bytes as usize / 16;
        // Pre-read the inserted lanes: `dst` may alias `src` or `ins`.
        let insv: Vec<Value> = (0..*num_lanes as usize)
            .map(|j| self.load_lane(*ins, j))
            .collect();
        for i in 0..n {
            let v = self.load_lane(*src, i);
            self.store_lane(*dst, i, v);
        }
        let base = *idx as usize * *num_lanes as usize;
        for (j, v) in insv.into_iter().enumerate() {
            self.store_lane(*dst, base + j, v);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_extract_lane_wide(
        &mut self,
        dst: &u8,
        src: &u8,
        idx: &u8,
        num_lanes: &u8,
    ) -> bool {
        let n = *num_lanes as usize;
        let base = *idx as usize * n;
        // Pre-read: `dst` may alias `src`.
        let ext: Vec<Value> = (0..n).map(|j| self.load_lane(*src, base + j)).collect();
        for (j, v) in ext.into_iter().enumerate() {
            self.store_lane(*dst, j, v);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_pcmp_str(&mut self, a: &u8, b: &u8, imm: &u8, explicit: &bool) -> bool {
        // Index + flags from the shared pcmpstr_run (out-slot, like BMI): the
        // helper is read-only on cpu, and the JIT stores ECX + flags itself so its
        // cached GPR/flag state stays coherent.
        let cpu = self.cpu;
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let im = self.iconst(*imm as u64);
        let ex = self.iconst(*explicit as u64);
        let (ss, _) = self.call_with_out_slot(self.helpers.pcmpstr, &[cpu, av, bv, im, ex]);
        let ecx = self.builder.ins().stack_load(types::I64, ss, 0);
        let flags = self.builder.ins().stack_load(types::I64, ss, 8);
        self.write_gpr(1, ecx, 4); // ECX (zero-extends RCX)
        for (bit, off) in [
            (0i64, self.offsets.cf),
            (1, self.offsets.zf),
            (2, self.offsets.sf),
            (3, self.offsets.of),
        ] {
            let shifted = self.builder.ins().ushr_imm(flags, bit);
            let one = self.builder.ins().band_imm(shifted, 1);
            let fb = self.builder.ins().icmp_imm(IntCC::NotEqual, one, 0);
            self.store_flag(off, fb);
        }
        let z8 = self.builder.ins().iconst(types::I8, 0);
        self.store_flag(self.offsets.af, z8);
        self.store_flag(self.offsets.pf, z8);
        false
    }

    pub(crate) fn emit_v_pcmp_str_m(
        &mut self,
        a: &u8,
        addr: &Val,
        imm: &u8,
        explicit: &bool,
    ) -> bool {
        // Memory source 2: load the 128-bit operand (faults trap here), then run
        // the shared pcmpstr with the loaded value. Same out-slot ECX+flags path
        // as VPcmpStr — the helper is read-only on cpu.
        let base = self.val(*addr);
        let host = self.checked_addr(base, 16, 0);
        let lo = self.gload(types::I64, host, 0);
        let hi = self.gload(types::I64, host, 8);
        let cpu = self.cpu;
        let av = self.iconst(*a as u64);
        let im = self.iconst(*imm as u64);
        let ex = self.iconst(*explicit as u64);
        let (ss, _) = self.call_with_out_slot(self.helpers.pcmpstr_mem, &[cpu, av, lo, hi, im, ex]);
        let ecx = self.builder.ins().stack_load(types::I64, ss, 0);
        let flags = self.builder.ins().stack_load(types::I64, ss, 8);
        self.write_gpr(1, ecx, 4); // ECX (zero-extends RCX)
        for (bit, off) in [
            (0i64, self.offsets.cf),
            (1, self.offsets.zf),
            (2, self.offsets.sf),
            (3, self.offsets.of),
        ] {
            let shifted = self.builder.ins().ushr_imm(flags, bit);
            let one = self.builder.ins().band_imm(shifted, 1);
            let fb = self.builder.ins().icmp_imm(IntCC::NotEqual, one, 0);
            self.store_flag(off, fb);
        }
        let z8 = self.builder.ins().iconst(types::I8, 0);
        self.store_flag(self.offsets.af, z8);
        self.store_flag(self.offsets.pf, z8);
        false
    }

    pub(crate) fn emit_v_align(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        shift: &u8,
        elem: &u8,
        bytes: &u16,
    ) -> bool {
        // Cross-lane byte shift via the shared helper (low-frequency, jit==interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let sh = self.iconst(*shift as u64);
        let el = self.iconst(*elem as u64);
        let by = self.iconst(*bytes as u64);
        self.call_helper(self.helpers.valign, &[cpu, d, av, bv, sh, el, by]);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_perm_t2(
        &mut self,
        dst: &u8,
        idx: &u8,
        tbl: &u8,
        elem: &u8,
        writemask: &Option<u8>,
        zeroing: &bool,
        bytes: &u16,
        imode: &bool,
    ) -> bool {
        // Two-table cross-lane permute via the shared helper (cold + masked,
        // jit==interp). Writes the dst vector in CpuState (memory-backed).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let ix = self.iconst(*idx as u64);
        let tb = self.iconst(*tbl as u64);
        let el = self.iconst(*elem as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        let by = self.iconst(*bytes as u64);
        let im = self.iconst(*imode as u64);
        self.call_helper(
            self.helpers.vpermt2,
            &[cpu, d, ix, tb, el, k, masked, z, by, im],
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_perm_t2_m(
        &mut self,
        dst: &u8,
        idx: &u8,
        addr: &Val,
        elem: &u8,
        writemask: &Option<u8>,
        zeroing: &bool,
        bytes: &u16,
        imode: &bool,
    ) -> bool {
        // Memory-source table 1 via the fault-capable helper (flush GPRs, then
        // trap on an unmapped load). Vector state is memory-backed.
        let cpu = self.cpu;
        let mem = self.mem;
        let base = self.val(*addr);
        let d = self.iconst(*dst as u64);
        let ix = self.iconst(*idx as u64);
        let el = self.iconst(*elem as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        let by = self.iconst(*bytes as u64);
        let im = self.iconst(*imode as u64);
        let cur = self.iconst(self.cur_addr);
        self.flush_gprs();
        let inst = self.call_helper(
            self.helpers.vpermt2_mem,
            &[cpu, mem, d, ix, base, el, k, masked, z, by, im, cur],
        );
        self.trap_if_unmapped(inst);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_perm1(
        &mut self,
        dst: &u8,
        idx: &u8,
        src: &u8,
        elem: &u8,
        bytes: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // Single-source cross-lane permute via the shared helper (jit == interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let ix = self.iconst(*idx as u64);
        let s = self.iconst(*src as u64);
        let el = self.iconst(*elem as u64);
        let by = self.iconst(*bytes as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(self.helpers.vperm1, &[cpu, d, ix, s, el, by, k, masked, z]);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_masked_logic(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        op: &VLogicOp,
        k: &u8,
        elem: &u8,
        zeroing: &bool,
        bytes: &u16,
    ) -> bool {
        // Compute + masked writeback via the shared helper (like VMaskMov): masked
        // ops aren't hot, and delegating to write_masked guarantees jit == interp.
        let op_code = match op {
            VLogicOp::Xor => 0u64,
            VLogicOp::And => 1,
            VLogicOp::Or => 2,
            VLogicOp::Andn => 3,
        };
        let cpu = self.cpu;
        let oc = self.iconst(op_code);
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let kk = self.iconst(*k as u64);
        let el = self.iconst(*elem as u64);
        let z = self.iconst(*zeroing as u64);
        let by = self.iconst(*bytes as u64);
        self.call_helper(
            self.helpers.vmasked_logic,
            &[cpu, oc, d, av, bv, kk, el, z, by],
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_masked_packed(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        op: &PackedBinOp,
        k: &u8,
        elem: &u8,
        zeroing: &bool,
        bytes: &u16,
    ) -> bool {
        // Compute + masked writeback via the shared helper (like VMaskedLogic):
        // masked ops aren't hot, and write_masked guarantees jit == interp.
        let op_code = match op {
            PackedBinOp::Add => 0u64,
            PackedBinOp::Sub => 1,
            PackedBinOp::MinU => 2,
            PackedBinOp::MaxU => 3,
            PackedBinOp::MinS => 4,
            PackedBinOp::MaxS => 5,
            PackedBinOp::MulLo32 => 6,
            PackedBinOp::MulLo64 => 9,
            PackedBinOp::CmpEq => 7,
            PackedBinOp::CmpGt => 8,
        };
        let cpu = self.cpu;
        let oc = self.iconst(op_code);
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let kk = self.iconst(*k as u64);
        let el = self.iconst(*elem as u64);
        let z = self.iconst(*zeroing as u64);
        let by = self.iconst(*bytes as u64);
        self.call_helper(
            self.helpers.vmasked_packed,
            &[cpu, oc, d, av, bv, kk, el, z, by],
        );
        false
    }

    pub(crate) fn emit_v_logic256(&mut self, dst: &u8, a: &u8, b: &u8, op: &VLogicOp) -> bool {
        let (alo, blo) = (self.load_xmm(*a), self.load_xmm(*b));
        let rlo = self.emit_vlogic(alo, blo, *op);
        self.store_xmm(*dst, rlo);
        let (ahi, bhi) = (self.load_ymm_hi(*a), self.load_ymm_hi(*b));
        let rhi = self.emit_vlogic(ahi, bhi, *op);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_logic_wide(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        op: &VLogicOp,
        bytes: &u16,
    ) -> bool {
        let n = *bytes as usize / 16;
        for i in 0..n {
            let (av, bv) = (self.load_lane(*a, i), self.load_lane(*b, i));
            let r = self.emit_vlogic(av, bv, *op);
            self.store_lane(*dst, i, r);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_logic_wide_m(
        &mut self,
        dst: &u8,
        a: &u8,
        addr: &Val,
        op: &VLogicOp,
        bytes: &u16,
    ) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, *bytes as u8, 0);
        let n = *bytes as usize / 16;
        for i in 0..n {
            let av = self.load_lane(*a, i);
            let bv = self.gload(types::I128, host, (i * 16) as i32);
            let r = self.emit_vlogic(av, bv, *op);
            self.store_lane(*dst, i, r);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_popcnt(&mut self, dst: &u8, a: &u8, lane: &u8, bytes: &u16) -> bool {
        let n = *bytes as usize / 16;
        for i in 0..n {
            let v = self.load_lane(*a, i);
            let r = self.emit_vpopcnt(v, *lane);
            self.store_lane(*dst, i, r);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_popcnt_m(&mut self, dst: &u8, addr: &Val, lane: &u8, bytes: &u16) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, *bytes as u8, 0);
        let n = *bytes as usize / 16;
        for i in 0..n {
            let v = self.gload(types::I128, host, (i * 16) as i32);
            let r = self.emit_vpopcnt(v, *lane);
            self.store_lane(*dst, i, r);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_p_mov_extend(
        &mut self,
        dst: &u8,
        src: &u8,
        from: &u8,
        to: &u8,
        signed: &bool,
    ) -> bool {
        let s = self.load_xmm(*src);
        let r = self.emit_pmov_extend(s, *from, *to, *signed);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_p_mov_extend_m(
        &mut self,
        dst: &u8,
        addr: &Val,
        from: &u8,
        to: &u8,
        signed: &bool,
    ) -> bool {
        let nbytes = (16 / *to as usize) * *from as usize;
        let av = self.val(*addr);
        let host = self.checked_addr(av, nbytes as u8, 0);
        let load_ty = match nbytes {
            8 => types::I64,
            4 => types::I32,
            _ => types::I16, // bq: 2 bytes
        };
        let m = self.gload(load_ty, host, 0);
        let m128 = self.builder.ins().uextend(types::I128, m);
        let r = self.emit_pmov_extend(m128, *from, *to, *signed);
        self.store_xmm(*dst, r);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_p_mov_extend_wide(
        &mut self,
        dst: &u8,
        src: &u8,
        from: &u8,
        to: &u8,
        signed: &bool,
        dst_width: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // Wide/masked widening via the shared helper (cold + masked, jit == interp).
        // Writes the dst vector in CpuState (memory-backed).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let s = self.iconst(*src as u64);
        let fr = self.iconst(*from as u64);
        let t = self.iconst(*to as u64);
        let sg = self.iconst(*signed as u64);
        let dw = self.iconst(*dst_width as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.vpmov_extend_wide,
            &[cpu, d, s, fr, t, sg, dw, k, masked, z],
        );
        false
    }

    pub(crate) fn emit_v_p_abs(
        &mut self,
        dst: &u8,
        src: &u8,
        elem: &u8,
        dst_width: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // Packed abs via the shared helper (cold + masked, jit == interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let s = self.iconst(*src as u64);
        let el = self.iconst(*elem as u64);
        let dw = self.iconst(*dst_width as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(self.helpers.vpabs, &[cpu, d, s, el, dw, k, masked, z]);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_p_unary_lane(
        &mut self,
        dst: &u8,
        src: &u8,
        op: &VpUnaryOp,
        imm: &u8,
        elem: &u8,
        dst_width: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // Masked EVEX unary lane op via the shared helper (cold + masked, jit == interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let s = self.iconst(*src as u64);
        let o = self.iconst(*op as u64);
        let im = self.iconst(*imm as u64);
        let el = self.iconst(*elem as u64);
        let dw = self.iconst(*dst_width as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.vp_unary_lane,
            &[cpu, d, s, o, im, el, dw, k, masked, z],
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_p_blendm(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        k: &u8,
        elem: &u8,
        dst_width: &u16,
        zeroing: &bool,
    ) -> bool {
        // Masked EVEX blend via the shared helper (cold + masked, jit == interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let kk = self.iconst(*k as u64);
        let el = self.iconst(*elem as u64);
        let dw = self.iconst(*dst_width as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(self.helpers.vp_blendm, &[cpu, d, av, bv, kk, el, dw, z]);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_shuf_lane(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        imm: &u8,
        elem: &u8,
        dst_width: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // Masked EVEX 128-bit-lane shuffle via the shared helper (cold, jit == interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let im = self.iconst(*imm as u64);
        let el = self.iconst(*elem as u64);
        let dw = self.iconst(*dst_width as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.vshuf_lane,
            &[cpu, d, av, bv, im, el, dw, k, masked, z],
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_p_multishift(
        &mut self,
        dst: &u8,
        ctrl: &u8,
        data: &u8,
        dst_width: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // Masked EVEX vpmultishiftqb via the shared helper (cold, jit == interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let c = self.iconst(*ctrl as u64);
        let dt = self.iconst(*data as u64);
        let dw = self.iconst(*dst_width as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.vp_multishift,
            &[cpu, d, c, dt, dw, k, masked, z],
        );
        false
    }

    pub(crate) fn emit_v_p_blend_v(&mut self, dst: &u8, src: &u8, lane: &u8) -> bool {
        let (d, s, m) = (self.load_xmm(*dst), self.load_xmm(*src), self.load_xmm(0));
        let r = self.emit_blendv(d, s, m, *lane);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_p_blend_v_m(&mut self, dst: &u8, addr: &Val, lane: &u8) -> bool {
        let av = self.val(*addr);
        let host = self.checked_addr(av, 16, 0);
        let s = self.gload(types::I128, host, 0);
        let (d, m) = (self.load_xmm(*dst), self.load_xmm(0));
        let r = self.emit_blendv(d, s, m, *lane);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_p_round(
        &mut self,
        dst: &u8,
        a: &u8,
        src: &u8,
        prec: &FPrec,
        mode: &u8,
        scalar: &bool,
    ) -> bool {
        let (av, s) = (self.load_xmm(*a), self.load_xmm(*src));
        let r = self.emit_round(av, s, *prec, *mode, *scalar);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_p_round_m(
        &mut self,
        dst: &u8,
        addr: &Val,
        prec: &FPrec,
        mode: &u8,
        scalar: &bool,
    ) -> bool {
        let size = if *scalar { prec.bytes() } else { 16 };
        let av = self.val(*addr);
        let host = self.checked_addr(av, size, 0);
        // Scalar loads one element into the low lane; packed loads the full 128.
        let s = if *scalar && prec.bytes() == 8 {
            let e = self.gload(types::I64, host, 0);
            self.builder.ins().uextend(types::I128, e)
        } else if *scalar {
            let e = self.gload(types::I32, host, 0);
            self.builder.ins().uextend(types::I128, e)
        } else {
            self.gload(types::I128, host, 0)
        };
        let d = self.load_xmm(*dst);
        let r = self.emit_round(d, s, *prec, *mode, *scalar);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_p_ternlog(
        &mut self,
        dst: &u8,
        b: &u8,
        c: &u8,
        imm: &u8,
        bytes: &u16,
    ) -> bool {
        let n = *bytes as usize / 16;
        for i in 0..n {
            // `dst` is also the first source.
            let (av, bv, cv) = (
                self.load_lane(*dst, i),
                self.load_lane(*b, i),
                self.load_lane(*c, i),
            );
            let r = self.emit_ternlog(av, bv, cv, *imm);
            self.store_lane(*dst, i, r);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_p_ternlog_m(
        &mut self,
        dst: &u8,
        b: &u8,
        addr: &Val,
        imm: &u8,
        bytes: &u16,
    ) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, *bytes as u8, 0);
        let n = *bytes as usize / 16;
        for i in 0..n {
            // `dst` is also the first source; `c` comes from memory.
            let av = self.load_lane(*dst, i);
            let bv = self.load_lane(*b, i);
            let cv = self.gload(types::I128, host, (i * 16) as i32);
            let r = self.emit_ternlog(av, bv, cv, *imm);
            self.store_lane(*dst, i, r);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_logic256_m(
        &mut self,
        dst: &u8,
        a: &u8,
        addr: &Val,
        op: &VLogicOp,
    ) -> bool {
        let av = self.val(*addr);
        let host = self.checked_addr(av, 32, 0);
        let mlo = self.gload(types::I128, host, 0);
        let mhi = self.gload(types::I128, host, 16);
        let alo = self.load_xmm(*a);
        let rlo = self.emit_vlogic(alo, mlo, *op);
        self.store_xmm(*dst, rlo);
        let ahi = self.load_ymm_hi(*a);
        let rhi = self.emit_vlogic(ahi, mhi, *op);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_packed_bin256(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        lane: &u8,
        op: &PackedBinOp,
    ) -> bool {
        let vty = vec_ty(*lane);
        let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
        let (va, vb) = (self.bitcast_v(xa, vty), self.bitcast_v(xb, vty));
        let rlo = self.emit_packed_bin(va, vb, *op);
        let rlo = self.bitcast_i128(rlo);
        self.store_xmm(*dst, rlo);
        let (ya, yb) = (self.load_ymm_hi(*a), self.load_ymm_hi(*b));
        let (va, vb) = (self.bitcast_v(ya, vty), self.bitcast_v(yb, vty));
        let rhi = self.emit_packed_bin(va, vb, *op);
        let rhi = self.bitcast_i128(rhi);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_packed_bin256_m(
        &mut self,
        dst: &u8,
        a: &u8,
        addr: &Val,
        lane: &u8,
        op: &PackedBinOp,
    ) -> bool {
        let av = self.val(*addr);
        let host = self.checked_addr(av, 32, 0);
        let (mlo, mhi) = (
            self.gload(types::I128, host, 0),
            self.gload(types::I128, host, 16),
        );
        let vty = vec_ty(*lane);
        let xa = self.load_xmm(*a);
        let (va, vm) = (self.bitcast_v(xa, vty), self.bitcast_v(mlo, vty));
        let rlo = self.emit_packed_bin(va, vm, *op);
        let rlo = self.bitcast_i128(rlo);
        self.store_xmm(*dst, rlo);
        let ya = self.load_ymm_hi(*a);
        let (va, vm) = (self.bitcast_v(ya, vty), self.bitcast_v(mhi, vty));
        let rhi = self.emit_packed_bin(va, vm, *op);
        let rhi = self.bitcast_i128(rhi);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_packed_wide(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        lane: &u8,
        op: &PackedBinOp,
        bytes: &u16,
    ) -> bool {
        let vty = vec_ty(*lane);
        let n = *bytes as usize / 16;
        for i in 0..n {
            let (xa, xb) = (self.load_lane(*a, i), self.load_lane(*b, i));
            let (va, vb) = (self.bitcast_v(xa, vty), self.bitcast_v(xb, vty));
            let r = self.emit_packed_bin(va, vb, *op);
            let r = self.bitcast_i128(r);
            self.store_lane(*dst, i, r);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_packed_wide_m(
        &mut self,
        dst: &u8,
        a: &u8,
        addr: &Val,
        lane: &u8,
        op: &PackedBinOp,
        bytes: &u16,
    ) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, *bytes as u8, 0);
        let vty = vec_ty(*lane);
        let n = *bytes as usize / 16;
        for i in 0..n {
            let xa = self.load_lane(*a, i);
            let m = self.gload(types::I128, host, (i * 16) as i32);
            let (va, vm) = (self.bitcast_v(xa, vty), self.bitcast_v(m, vty));
            let r = self.emit_packed_bin(va, vm, *op);
            let r = self.bitcast_i128(r);
            self.store_lane(*dst, i, r);
        }
        self.store_lanes_zeroed_above(*dst, n);
        false
    }

    pub(crate) fn emit_v_move_mask_b256(&mut self, dst: &u32, src: &u8) -> bool {
        let lo = self.load_xmm(*src);
        let vlo = self.bitcast_v(lo, types::I8X16);
        let mlo = self.builder.ins().vhigh_bits(types::I32, vlo);
        let hi = self.load_ymm_hi(*src);
        let vhi = self.bitcast_v(hi, types::I8X16);
        let mhi = self.builder.ins().vhigh_bits(types::I32, vhi);
        let mhi = self.builder.ins().ishl_imm(mhi, 16);
        let combined = self.builder.ins().bor(mlo, mhi);
        let r = self.builder.ins().uextend(types::I64, combined);
        self.set(*dst, r);
        false
    }

    pub(crate) fn emit_v_broadcast_gpr(
        &mut self,
        dst: &u8,
        src: &Val,
        elem: &u8,
        width: &u16,
    ) -> bool {
        let (_ety, vty) = broadcast_types(*elem);
        let val = self.val(*src);
        let e = self.narrow(val, *elem);
        let splat = self.builder.ins().splat(vty, e);
        let v = self.bitcast_i128(splat);
        let z = self.builder.ins().iconst(types::I64, 0);
        let z128 = self.builder.ins().uextend(types::I128, z);
        self.store_xmm(*dst, v);
        self.store_ymm_hi(*dst, if *width >= 32 { v } else { z128 });
        let hi = if *width >= 64 { v } else { z128 };
        self.store_zmm_hi(*dst, 0, hi);
        self.store_zmm_hi(*dst, 1, hi);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_p_cmp_to_mask(
        &mut self,
        k: &u8,
        a: &u8,
        b: &u8,
        elem: &u8,
        width: &u16,
        pred: &u8,
        signed: &bool,
        writemask: &Option<u8>,
    ) -> bool {
        self.emit_vpcmp_to_mask(*k, *a, *b, None, *elem, *width, *pred, *signed, *writemask);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_p_cmp_to_mask_m(
        &mut self,
        k: &u8,
        a: &u8,
        addr: &Val,
        elem: &u8,
        width: &u16,
        pred: &u8,
        signed: &bool,
        writemask: &Option<u8>,
    ) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, *width as u8, 0);
        self.emit_vpcmp_to_mask(
            *k,
            *a,
            0,
            Some(host),
            *elem,
            *width,
            *pred,
            *signed,
            *writemask,
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_p_test_to_mask(
        &mut self,
        k: &u8,
        a: &u8,
        b: &u8,
        elem: &u8,
        width: &u16,
        neg: &bool,
        writemask: &Option<u8>,
    ) -> bool {
        self.emit_vptest_to_mask(*k, *a, *b, None, *elem, *width, *neg, *writemask);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_p_test_to_mask_m(
        &mut self,
        k: &u8,
        a: &u8,
        addr: &Val,
        elem: &u8,
        width: &u16,
        neg: &bool,
        writemask: &Option<u8>,
    ) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, *width as u8, 0);
        self.emit_vptest_to_mask(*k, *a, 0, Some(host), *elem, *width, *neg, *writemask);
        false
    }

    pub(crate) fn emit_v_k_or_test(&mut self, a: &u8, b: &u8, width: &u8) -> bool {
        let wmask = if *width >= 64 {
            u64::MAX
        } else {
            (1u64 << *width) - 1
        };
        let ka = self.load_cpu(self.offsets.kmask(*a as usize));
        let kb = self.load_cpu(self.offsets.kmask(*b as usize));
        let t = self.builder.ins().bor(ka, kb);
        let wm = self.builder.ins().iconst(types::I64, wmask as i64);
        let t = self.builder.ins().band(t, wm);
        let zero = self.builder.ins().iconst(types::I64, 0);
        let zf = self.builder.ins().icmp(IntCC::Equal, t, zero);
        let cf = self.builder.ins().icmp(IntCC::Equal, t, wm);
        self.store_flag(self.offsets.zf, zf);
        self.store_flag(self.offsets.cf, cf);
        let z8 = self.builder.ins().iconst(types::I8, 0);
        for off in [
            self.offsets.of,
            self.offsets.sf,
            self.offsets.af,
            self.offsets.pf,
        ] {
            self.store_flag(off, z8);
        }
        false
    }

    pub(crate) fn emit_v_k_from_gpr(&mut self, k: &u8, src: &Val, width: &u8) -> bool {
        let v = self.val(*src);
        let m = self.mask_kwidth(v, *width);
        self.store_cpu(self.offsets.kmask(*k as usize), m);
        false
    }

    pub(crate) fn emit_v_k_to_gpr(&mut self, dst: &u32, k: &u8, width: &u8) -> bool {
        let v = self.load_cpu(self.offsets.kmask(*k as usize));
        let m = self.mask_kwidth(v, *width);
        self.set(*dst, m);
        false
    }

    pub(crate) fn emit_v_k_mov_k_k(&mut self, dst: &u8, src: &u8, width: &u8) -> bool {
        let v = self.load_cpu(self.offsets.kmask(*src as usize));
        let m = self.mask_kwidth(v, *width);
        self.store_cpu(self.offsets.kmask(*dst as usize), m);
        false
    }

    pub(crate) fn emit_v_k_unpack(&mut self, dst: &u8, a: &u8, b: &u8, half: &u8) -> bool {
        let va = self.load_cpu(self.offsets.kmask(*a as usize));
        let vb = self.load_cpu(self.offsets.kmask(*b as usize));
        let lo = self.mask_kwidth(vb, *half);
        let hi_masked = self.mask_kwidth(va, *half);
        let hi = self.builder.ins().ishl_imm(hi_masked, *half as i64);
        let r = self.builder.ins().bor(hi, lo);
        self.store_cpu(self.offsets.kmask(*dst as usize), r);
        false
    }

    pub(crate) fn emit_v_k_bin_op(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        op: &VKLogicOp,
        width: &u8,
    ) -> bool {
        let ka = self.load_cpu(self.offsets.kmask(*a as usize));
        let kb = self.load_cpu(self.offsets.kmask(*b as usize));
        let r = match op {
            VKLogicOp::Or => self.builder.ins().bor(ka, kb),
            VKLogicOp::And => self.builder.ins().band(ka, kb),
            // x86 kandn: `~SRC1 & SRC2` = `kb & ~ka` = band_not(kb, ka).
            VKLogicOp::Andn => self.builder.ins().band_not(kb, ka),
            VKLogicOp::Xor => self.builder.ins().bxor(ka, kb),
            VKLogicOp::Xnor => {
                let x = self.builder.ins().bxor(ka, kb);
                self.builder.ins().bnot(x)
            }
        };
        let m = self.mask_kwidth(r, *width);
        self.store_cpu(self.offsets.kmask(*dst as usize), m);
        false
    }

    pub(crate) fn emit_v_k_not(&mut self, dst: &u8, a: &u8, width: &u8) -> bool {
        let ka = self.load_cpu(self.offsets.kmask(*a as usize));
        let n = self.builder.ins().bnot(ka);
        let m = self.mask_kwidth(n, *width);
        self.store_cpu(self.offsets.kmask(*dst as usize), m);
        false
    }

    pub(crate) fn emit_v_k_shift(
        &mut self,
        dst: &u8,
        a: &u8,
        amount: &u8,
        width: &u8,
        left: &bool,
    ) -> bool {
        let ka = self.load_cpu(self.offsets.kmask(*a as usize));
        let s = self.mask_kwidth(ka, *width);
        // A shift ≥ 64 is UB in Cranelift; bake the imm result to 0 instead.
        let r = if *amount >= 64 {
            self.iconst(0)
        } else if *left {
            let sh = self.builder.ins().ishl_imm(s, *amount as i64);
            self.mask_kwidth(sh, *width)
        } else {
            self.builder.ins().ushr_imm(s, *amount as i64)
        };
        self.store_cpu(self.offsets.kmask(*dst as usize), r);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_pmov_narrow(
        &mut self,
        dst: &u8,
        src: &u8,
        from: &u8,
        to: &u8,
        src_width: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // Narrowing move via the shared helper (cold + masked, jit == interp).
        // Writes the dst vector in CpuState (memory-backed).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let s = self.iconst(*src as u64);
        let fr = self.iconst(*from as u64);
        let t = self.iconst(*to as u64);
        let sw = self.iconst(*src_width as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.vpmov_narrow,
            &[cpu, d, s, fr, t, sw, k, masked, z],
        );
        false
    }

    pub(crate) fn emit_v_pmov_narrow_mem(
        &mut self,
        src: &u8,
        addr: &Val,
        from: &u8,
        to: &u8,
        src_width: &u16,
    ) -> bool {
        // Narrowing store to memory via the fault-capable helper (like the masked
        // memory move): flush GPRs, then trap on an unmapped store.
        let cpu = self.cpu;
        let mem = self.mem;
        let base = self.val(*addr);
        let s = self.iconst(*src as u64);
        let fr = self.iconst(*from as u64);
        let t = self.iconst(*to as u64);
        let sw = self.iconst(*src_width as u64);
        let cur = self.iconst(self.cur_addr);
        self.flush_gprs();
        let inst = self.call_helper(
            self.helpers.vpmov_narrow_mem,
            &[cpu, mem, s, base, fr, t, sw, cur],
        );
        self.trap_if_unmapped(inst);
        false
    }

    pub(crate) fn emit_v_broadcast(&mut self, dst: &u8, src: &u8, elem: &u8, w256: &bool) -> bool {
        let x = self.load_xmm(*src);
        let (ety, vty) = broadcast_types(*elem);
        let e = self.builder.ins().ireduce(ety, x);
        let splat = self.builder.ins().splat(vty, e);
        let v = self.bitcast_i128(splat);
        self.store_xmm(*dst, v);
        if *w256 {
            self.store_ymm_hi(*dst, v);
        } else {
            self.store_ymm_hi_zero(*dst);
        }
        false
    }

    pub(crate) fn emit_v_broadcast_m(
        &mut self,
        dst: &u8,
        addr: &Val,
        elem: &u8,
        w256: &bool,
    ) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, *elem, 0);
        let (ety, vty) = broadcast_types(*elem);
        let e = self.gload(ety, host, 0);
        let splat = self.builder.ins().splat(vty, e);
        let v = self.bitcast_i128(splat);
        self.store_xmm(*dst, v);
        if *w256 {
            self.store_ymm_hi(*dst, v);
        } else {
            self.store_ymm_hi_zero(*dst);
        }
        false
    }

    pub(crate) fn emit_v_insert128(&mut self, dst: &u8, src: &u8, ins: &u8, hi: &bool) -> bool {
        let (slo, shi) = (self.load_xmm(*src), self.load_ymm_hi(*src));
        let insv = self.load_xmm(*ins);
        if *hi {
            self.store_xmm(*dst, slo);
            self.store_ymm_hi(*dst, insv);
        } else {
            self.store_xmm(*dst, insv);
            self.store_ymm_hi(*dst, shi);
        }
        false
    }

    pub(crate) fn emit_v_insert128_m(&mut self, dst: &u8, src: &u8, addr: &Val, hi: &bool) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, 16, 0);
        let insv = self.gload(types::I128, host, 0);
        let (slo, shi) = (self.load_xmm(*src), self.load_ymm_hi(*src));
        if *hi {
            self.store_xmm(*dst, slo);
            self.store_ymm_hi(*dst, insv);
        } else {
            self.store_xmm(*dst, insv);
            self.store_ymm_hi(*dst, shi);
        }
        false
    }

    pub(crate) fn emit_v_extract128(&mut self, dst: &u8, src: &u8, hi: &bool) -> bool {
        let v = if *hi {
            self.load_ymm_hi(*src)
        } else {
            self.load_xmm(*src)
        };
        self.store_xmm(*dst, v);
        self.store_ymm_hi_zero(*dst);
        false
    }

    pub(crate) fn emit_v_from_gpr(&mut self, dst: &u8, src: &Val, size: &u8) -> bool {
        let v = self.val(*src);
        let vm = self.mask(v, *size);
        let x = self.builder.ins().uextend(types::I128, vm);
        self.store_xmm(*dst, x);
        false
    }

    pub(crate) fn emit_v_to_gpr(&mut self, dst: &u32, src: &u8, size: &u8) -> bool {
        let v = self.load_xmm(*src);
        let lo = self.builder.ins().ireduce(types::I64, v);
        let r = self.mask(lo, *size);
        self.set(*dst, r);
        false
    }

    pub(crate) fn emit_v_logic(&mut self, dst: &u8, a: &u8, b: &u8, op: &VLogicOp) -> bool {
        let (va, vb) = (self.load_xmm(*a), self.load_xmm(*b));
        let r = self.emit_vlogic(va, vb, *op);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_packed_bin(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        lane: &u8,
        op: &PackedBinOp,
    ) -> bool {
        let vty = vec_ty(*lane);
        let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
        let va = self.bitcast_v(xa, vty);
        let vb = self.bitcast_v(xb, vty);
        let r = self.emit_packed_bin(va, vb, *op);
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_packed_bin_m(
        &mut self,
        dst: &u8,
        addr: &Val,
        lane: &u8,
        op: &PackedBinOp,
    ) -> bool {
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

    pub(crate) fn emit_v_logic_m(&mut self, dst: &u8, addr: &Val, op: &VLogicOp) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, 16, 0);
        let memv = self.gload(types::I128, host, 0);
        let vd = self.load_xmm(*dst);
        let r = self.emit_vlogic(vd, memv, *op);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_packed_shift(
        &mut self,
        dst: &u8,
        a: &u8,
        imm: &u8,
        lane: &u8,
        right: &bool,
        arith: &bool,
    ) -> bool {
        let xa = self.load_xmm(*a);
        let r = self.emit_packed_shift_imm(xa, *imm, *lane, *right, *arith);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_packed_shift256(
        &mut self,
        dst: &u8,
        a: &u8,
        imm: &u8,
        lane: &u8,
        right: &bool,
        arith: &bool,
    ) -> bool {
        let xa = self.load_xmm(*a);
        let rlo = self.emit_packed_shift_imm(xa, *imm, *lane, *right, *arith);
        self.store_xmm(*dst, rlo);
        let ya = self.load_ymm_hi(*a);
        let rhi = self.emit_packed_shift_imm(ya, *imm, *lane, *right, *arith);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_permq(&mut self, dst: &u8, src: &u8, imm: &u8) -> bool {
        let (xlo, xhi) = (self.load_xmm(*src), self.load_ymm_hi(*src));
        let lo_v = self.bitcast_v(xlo, types::I64X2);
        let hi_v = self.bitcast_v(xhi, types::I64X2);
        // Extract the four source quadwords (lane indices are compile-time).
        let q = [
            self.builder.ins().extractlane(lo_v, 0),
            self.builder.ins().extractlane(lo_v, 1),
            self.builder.ins().extractlane(hi_v, 0),
            self.builder.ins().extractlane(hi_v, 1),
        ];
        let sel = |i: u32| q[((*imm >> (2 * i)) & 3) as usize];
        let zero = self.builder.ins().iconst(types::I64, 0);
        let zv = self.builder.ins().splat(types::I64X2, zero);
        let lo0 = self.builder.ins().insertlane(zv, sel(0), 0);
        let lo1 = self.builder.ins().insertlane(lo0, sel(1), 1);
        let hi0 = self.builder.ins().insertlane(zv, sel(2), 0);
        let hi1 = self.builder.ins().insertlane(hi0, sel(3), 1);
        let rlo = self.bitcast_i128(lo1);
        let rhi = self.bitcast_i128(hi1);
        self.store_xmm(*dst, rlo);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_permd(&mut self, dst: &u8, ctrl: &u8, src: &u8) -> bool {
        self.emit_vpermd(*dst, *ctrl, *src);
        false
    }

    pub(crate) fn emit_v_perm2i128(&mut self, dst: &u8, a: &u8, b: &u8, imm: &u8) -> bool {
        let zero = self.builder.ins().iconst(types::I64, 0);
        let z128 = self.builder.ins().uextend(types::I128, zero);
        let halves = [
            self.load_xmm(*a),
            self.load_ymm_hi(*a),
            self.load_xmm(*b),
            self.load_ymm_hi(*b),
        ];
        let lane = |sel: u8| {
            if sel & 0x08 != 0 {
                z128
            } else {
                halves[(sel & 3) as usize]
            }
        };
        let rlo = lane(*imm);
        let rhi = lane(*imm >> 4);
        self.store_xmm(*dst, rlo);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_palignr256(&mut self, dst: &u8, a: &u8, b: &u8, imm: &u8) -> bool {
        let (alo, blo) = (self.load_xmm(*a), self.load_xmm(*b));
        let rlo = self.emit_palignr(alo, blo, *imm);
        self.store_xmm(*dst, rlo);
        let (ahi, bhi) = (self.load_ymm_hi(*a), self.load_ymm_hi(*b));
        let rhi = self.emit_palignr(ahi, bhi, *imm);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_ptest(&mut self, a: &u8, b: &u8, w256: &bool) -> bool {
        let zero = self.builder.ins().iconst(types::I64, 0);
        let z128 = self.builder.ins().uextend(types::I128, zero);
        let (alo, blo) = (self.load_xmm(*a), self.load_xmm(*b));
        let and_lo = self.builder.ins().band(blo, alo);
        let nalo = self.builder.ins().bnot(alo);
        let andn_lo = self.builder.ins().band(blo, nalo);
        let mut zf = self.builder.ins().icmp(IntCC::Equal, and_lo, z128);
        let mut cf = self.builder.ins().icmp(IntCC::Equal, andn_lo, z128);
        if *w256 {
            let (ahi, bhi) = (self.load_ymm_hi(*a), self.load_ymm_hi(*b));
            let and_hi = self.builder.ins().band(bhi, ahi);
            let nahi = self.builder.ins().bnot(ahi);
            let andn_hi = self.builder.ins().band(bhi, nahi);
            let zhi = self.builder.ins().icmp(IntCC::Equal, and_hi, z128);
            let chi = self.builder.ins().icmp(IntCC::Equal, andn_hi, z128);
            zf = self.builder.ins().band(zf, zhi);
            cf = self.builder.ins().band(cf, chi);
        }
        self.store_flag(self.offsets.zf, zf);
        self.store_flag(self.offsets.cf, cf);
        let z8 = self.builder.ins().iconst(types::I8, 0);
        for off in [
            self.offsets.of,
            self.offsets.sf,
            self.offsets.af,
            self.offsets.pf,
        ] {
            self.store_flag(off, z8);
        }
        false
    }

    pub(crate) fn emit_v_pshufb256(&mut self, dst: &u8, a: &u8, idx: &u8) -> bool {
        let (alo, ilo) = (self.load_xmm(*a), self.load_xmm(*idx));
        let rlo = self.emit_pshufb(alo, ilo);
        self.store_xmm(*dst, rlo);
        let (ahi, ihi) = (self.load_ymm_hi(*a), self.load_ymm_hi(*idx));
        let rhi = self.emit_pshufb(ahi, ihi);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_pshufb_wide(
        &mut self,
        dst: &u8,
        a: &u8,
        idx: &u8,
        bytes: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // EVEX per-lane byte shuffle via the shared helper (cold/masked, jit==interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let ix = self.iconst(*idx as u64);
        let by = self.iconst(*bytes as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.vpshufb_wide,
            &[cpu, d, av, ix, by, k, masked, z],
        );
        false
    }

    pub(crate) fn emit_v_pshufb256_m(&mut self, dst: &u8, a: &u8, addr: &Val) -> bool {
        let av = self.val(*addr);
        let host = self.checked_addr(av, 32, 0);
        let (ilo, ihi) = (
            self.gload(types::I128, host, 0),
            self.gload(types::I128, host, 16),
        );
        let alo = self.load_xmm(*a);
        let rlo = self.emit_pshufb(alo, ilo);
        self.store_xmm(*dst, rlo);
        let ahi = self.load_ymm_hi(*a);
        let rhi = self.emit_pshufb(ahi, ihi);
        self.store_ymm_hi(*dst, rhi);
        false
    }

    pub(crate) fn emit_v_byte_shift(&mut self, dst: &u8, a: &u8, bytes: &u8, right: &bool) -> bool {
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

    pub(crate) fn emit_v_shuffle32(&mut self, dst: &u8, a: &u8, imm: &u8) -> bool {
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

    pub(crate) fn emit_v_blend_w(&mut self, dst: &u8, a: &u8, b: &u8, imm: &u8) -> bool {
        // Per-word select via a byte shuffle: word i from a (bytes 2i,2i+1) or from
        // b (bytes 16+2i,16+2i+1) per imm8[i]. VEX.128 upper-zeroing is a trailing op.
        let mut mask = [0u8; 16];
        for i in 0..8usize {
            let base = if (imm >> i) & 1 != 0 {
                16 + 2 * i
            } else {
                2 * i
            };
            mask[2 * i] = base as u8;
            mask[2 * i + 1] = (base + 1) as u8;
        }
        let xa = self.load_xmm(*a);
        let xb = self.load_xmm(*b);
        let va = self.bitcast_v(xa, types::I8X16);
        let vb = self.bitcast_v(xb, types::I8X16);
        let r = self.shuffle(va, vb, mask);
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_fma(
        &mut self,
        dst: &u8,
        x: &u8,
        y: &u8,
        z: &u8,
        prec: &FPrec,
        scalar: &bool,
        neg_prod: &bool,
        neg_add: &bool,
        bytes: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // FMA via the shared helper (fused single-rounding, jit == interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let xv = self.iconst(*x as u64);
        let yv = self.iconst(*y as u64);
        let zv = self.iconst(*z as u64);
        let pf = self.iconst(matches!(prec, FPrec::F64) as u64);
        let sc = self.iconst(*scalar as u64);
        let np = self.iconst(*neg_prod as u64);
        let na = self.iconst(*neg_add as u64);
        let by = self.iconst(*bytes as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.fma,
            &[cpu, d, xv, yv, zv, pf, sc, np, na, by, k, masked, z],
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_fma_m(
        &mut self,
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
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // Memory-source FMA via the fault-capable helper (flush GPRs, trap on
        // unmapped load). Vector state is memory-backed.
        let cpu = self.cpu;
        let mem = self.mem;
        let base = self.val(*addr);
        let d = self.iconst(*dst as u64);
        let xv = self.iconst(*x as u64);
        let yv = self.iconst(*y as u64);
        let zv = self.iconst(*z as u64);
        let mr = self.iconst(*mem_role as u64);
        let pf = self.iconst(matches!(prec, FPrec::F64) as u64);
        let sc = self.iconst(*scalar as u64);
        let np = self.iconst(*neg_prod as u64);
        let na = self.iconst(*neg_add as u64);
        let by = self.iconst(*bytes as u64);
        let cur = self.iconst(self.cur_addr);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.flush_gprs();
        let inst = self.call_helper(
            self.helpers.fma_mem,
            &[
                cpu, mem, d, xv, yv, zv, base, mr, pf, sc, np, na, by, cur, k, masked, z,
            ],
        );
        self.trap_if_unmapped(inst);
        false
    }

    // --- EVEX lane broadcast (task-214). Register → `broadcast_lane` helper; memory →
    // the fault-capable `broadcast_lane_mem` helper (loads the chunk, traps on unmapped). ---

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_broadcast_lane(
        &mut self,
        dst: &u8,
        src: &u8,
        chunk: &u8,
        elem: &u8,
        dst_width: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let s = self.iconst(*src as u64);
        let ch = self.iconst(*chunk as u64);
        let el = self.iconst(*elem as u64);
        let dw = self.iconst(*dst_width as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.broadcast_lane,
            &[cpu, d, s, ch, el, dw, k, masked, z],
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_v_broadcast_lane_m(
        &mut self,
        dst: &u8,
        addr: &Val,
        chunk: &u8,
        elem: &u8,
        dst_width: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        let cpu = self.cpu;
        let mem = self.mem;
        let base = self.val(*addr);
        let d = self.iconst(*dst as u64);
        let ch = self.iconst(*chunk as u64);
        let el = self.iconst(*elem as u64);
        let dw = self.iconst(*dst_width as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        let cur = self.iconst(self.cur_addr);
        self.flush_gprs();
        let inst = self.call_helper(
            self.helpers.broadcast_lane_mem,
            &[cpu, mem, d, base, ch, el, dw, k, masked, z, cur],
        );
        self.trap_if_unmapped(inst);
        false
    }

    // --- AES-NI (task-205). Register form → `aes` helper; memory form loads the
    // 128-bit operand natively (checked_addr traps on unmapped) then calls `aes_mem`. ---

    pub(crate) fn emit_v_aes(&mut self, dst: &u8, a: &u8, b: &u8, op: &AesOp) -> bool {
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let o = self.iconst(*op as u64);
        let im = self.iconst(0);
        self.call_helper(self.helpers.aes, &[cpu, d, av, bv, o, im]);
        false
    }

    pub(crate) fn emit_v_aes_m(&mut self, dst: &u8, a: &u8, addr: &Val, op: &AesOp) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, 16, 0);
        let lo = self.gload(types::I64, host, 0);
        let hi = self.gload(types::I64, host, 8);
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let o = self.iconst(*op as u64);
        let im = self.iconst(0);
        self.call_helper(self.helpers.aes_mem, &[cpu, d, av, lo, hi, o, im]);
        false
    }

    pub(crate) fn emit_v_aes_imc(&mut self, dst: &u8, src: &u8) -> bool {
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let sv = self.iconst(*src as u64);
        let o = self.iconst(4); // imc
        let z = self.iconst(0);
        self.call_helper(self.helpers.aes, &[cpu, d, sv, z, o, z]);
        false
    }

    pub(crate) fn emit_v_aes_imc_m(&mut self, dst: &u8, addr: &Val) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, 16, 0);
        let lo = self.gload(types::I64, host, 0);
        let hi = self.gload(types::I64, host, 8);
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let z = self.iconst(0);
        let o = self.iconst(4); // imc
        self.call_helper(self.helpers.aes_mem, &[cpu, d, z, lo, hi, o, z]);
        false
    }

    pub(crate) fn emit_v_aes_keygen(&mut self, dst: &u8, src: &u8, imm: &u8) -> bool {
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let sv = self.iconst(*src as u64);
        let o = self.iconst(5); // keygen
        let im = self.iconst(*imm as u64);
        let z = self.iconst(0);
        self.call_helper(self.helpers.aes, &[cpu, d, sv, z, o, im]);
        false
    }

    pub(crate) fn emit_v_aes_keygen_m(&mut self, dst: &u8, addr: &Val, imm: &u8) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, 16, 0);
        let lo = self.gload(types::I64, host, 0);
        let hi = self.gload(types::I64, host, 8);
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let z = self.iconst(0);
        let o = self.iconst(5); // keygen
        let im = self.iconst(*imm as u64);
        self.call_helper(self.helpers.aes_mem, &[cpu, d, z, lo, hi, o, im]);
        false
    }

    // --- SHA-NI (task-207). Register form → `sha` helper; memory form loads the
    // 128-bit op2 natively (checked_addr traps on unmapped) then calls `sha_mem`.
    // `sha256rnds2` reads xmm0 implicitly inside the helper. ---

    pub(crate) fn emit_v_sha(&mut self, dst: &u8, a: &u8, b: &u8, imm: &u8, op: &ShaOp) -> bool {
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let o = self.iconst(*op as u64);
        let im = self.iconst(*imm as u64);
        self.call_helper(self.helpers.sha, &[cpu, d, av, bv, o, im]);
        false
    }

    pub(crate) fn emit_v_sha_m(
        &mut self,
        dst: &u8,
        a: &u8,
        addr: &Val,
        imm: &u8,
        op: &ShaOp,
    ) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, 16, 0);
        let lo = self.gload(types::I64, host, 0);
        let hi = self.gload(types::I64, host, 8);
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let o = self.iconst(*op as u64);
        let im = self.iconst(*imm as u64);
        self.call_helper(self.helpers.sha_mem, &[cpu, d, av, lo, hi, o, im]);
        false
    }

    // --- GFNI (task-210). Register form → `gfni` helper; memory form loads the
    // 128-bit op2 natively (checked_addr traps on unmapped) then calls `gfni_mem`. ---

    pub(crate) fn emit_v_gfni(&mut self, dst: &u8, a: &u8, b: &u8, imm: &u8, op: &GfniOp) -> bool {
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let o = self.iconst(*op as u64);
        let im = self.iconst(*imm as u64);
        self.call_helper(self.helpers.gfni, &[cpu, d, av, bv, o, im]);
        false
    }

    pub(crate) fn emit_v_gfni_m(
        &mut self,
        dst: &u8,
        a: &u8,
        addr: &Val,
        imm: &u8,
        op: &GfniOp,
    ) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, 16, 0);
        let lo = self.gload(types::I64, host, 0);
        let hi = self.gload(types::I64, host, 8);
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let o = self.iconst(*op as u64);
        let im = self.iconst(*imm as u64);
        self.call_helper(self.helpers.gfni_mem, &[cpu, d, av, lo, hi, o, im]);
        false
    }

    // --- PCLMULQDQ (task-211). Register form → `pclmul` helper; memory form loads the
    // 128-bit op2 natively (checked_addr traps on unmapped) then calls `pclmul_mem`. ---

    pub(crate) fn emit_v_pclmul(&mut self, dst: &u8, a: &u8, b: &u8, imm: &u8) -> bool {
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let im = self.iconst(*imm as u64);
        self.call_helper(self.helpers.pclmul, &[cpu, d, av, bv, im]);
        false
    }

    pub(crate) fn emit_v_pclmul_m(&mut self, dst: &u8, a: &u8, addr: &Val, imm: &u8) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, 16, 0);
        let lo = self.gload(types::I64, host, 0);
        let hi = self.gload(types::I64, host, 8);
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let im = self.iconst(*imm as u64);
        self.call_helper(self.helpers.pclmul_mem, &[cpu, d, av, lo, hi, im]);
        false
    }

    // --- MMX↔XMM bridge (task-208). Both forms → the shared `mmx_bridge` helper
    // (touches cpu.xmm/cpu.fpr, memory-backed). `op`: 0 = movq2dq, 1 = movdq2q. ---

    pub(crate) fn emit_movq2dq(&mut self, dst: &u8, src_mm: &u8) -> bool {
        let cpu = self.cpu;
        let op = self.iconst(0);
        let a = self.iconst(*dst as u64);
        let b = self.iconst(*src_mm as u64);
        self.call_helper(self.helpers.mmx_bridge, &[cpu, op, a, b]);
        false
    }

    pub(crate) fn emit_movdq2q(&mut self, dst_mm: &u8, src_xmm: &u8) -> bool {
        let cpu = self.cpu;
        let op = self.iconst(1);
        let a = self.iconst(*dst_mm as u64);
        let b = self.iconst(*src_xmm as u64);
        self.call_helper(self.helpers.mmx_bridge, &[cpu, op, a, b]);
        false
    }

    // --- SSSE3 psign (task-210). Pure element-wise codegen (no helper):
    // `dst[i] = ctrl[i] < 0 ? -src[i] : (ctrl[i] == 0 ? 0 : src[i])`. ---

    /// Emit the psign transform on two i128 values at `lane`-byte granularity, returning
    /// the i128 result. Built from vector `icmp` masks + `bitselect`: pick `-src` where
    /// `ctrl < 0`, then zero where `ctrl == 0`, else keep `src`.
    fn emit_psign(&mut self, src: Value, ctrl: Value, lane: u8) -> Value {
        let ty = match lane {
            1 => types::I8X16,
            2 => types::I16X8,
            _ => types::I32X4,
        };
        let s = self.bitcast_v(src, ty);
        let c = self.bitcast_v(ctrl, ty);
        let zero = self.builder.ins().iconst(ty.lane_type(), 0);
        let zeros = self.builder.ins().splat(ty, zero);
        let neg = self.builder.ins().ineg(s);
        // ctrl < 0 ? neg : src
        let ltz = self.builder.ins().icmp(IntCC::SignedLessThan, c, zeros);
        let pick = self.builder.ins().bitselect(ltz, neg, s);
        // ctrl == 0 ? 0 : pick
        let eqz = self.builder.ins().icmp(IntCC::Equal, c, zeros);
        let r = self.builder.ins().bitselect(eqz, zeros, pick);
        self.bitcast_i128(r)
    }

    pub(crate) fn emit_v_psign(&mut self, dst: &u8, a: &u8, b: &u8, lane: &u8) -> bool {
        let (src, ctrl) = (self.load_xmm(*a), self.load_xmm(*b));
        let r = self.emit_psign(src, ctrl, *lane);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_psign_m(&mut self, dst: &u8, a: &u8, addr: &Val, lane: &u8) -> bool {
        let base = self.val(*addr);
        let host = self.checked_addr(base, 16, 0);
        let ctrl = self.gload(types::I128, host, 0);
        let src = self.load_xmm(*a);
        let r = self.emit_psign(src, ctrl, *lane);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_pack_wide(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        from_elem: &u8,
        signed: &bool,
        bytes: &u16,
    ) -> bool {
        // Saturating pack via the shared helper (cold, jit == interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let bv = self.iconst(*b as u64);
        let fe = self.iconst(*from_elem as u64);
        let sg = self.iconst(*signed as u64);
        let by = self.iconst(*bytes as u64);
        self.call_helper(self.helpers.vpack, &[cpu, d, av, bv, fe, sg, by]);
        false
    }

    pub(crate) fn emit_v_shuffle32_wide(
        &mut self,
        dst: &u8,
        a: &u8,
        imm: &u8,
        bytes: &u16,
        writemask: &Option<u8>,
        zeroing: &bool,
    ) -> bool {
        // EVEX/VEX-256 per-lane dword shuffle via the shared helper (jit==interp).
        let cpu = self.cpu;
        let d = self.iconst(*dst as u64);
        let av = self.iconst(*a as u64);
        let im = self.iconst(*imm as u64);
        let by = self.iconst(*bytes as u64);
        let k = self.iconst(writemask.unwrap_or(0) as u64);
        let masked = self.iconst(writemask.is_some() as u64);
        let z = self.iconst(*zeroing as u64);
        self.call_helper(
            self.helpers.vshuffle32_wide,
            &[cpu, d, av, im, by, k, masked, z],
        );
        false
    }

    pub(crate) fn emit_v_move_half(
        &mut self,
        dst: &u8,
        src: &u8,
        dst_high: &bool,
        src_high: &bool,
    ) -> bool {
        let (xs, xd) = (self.load_xmm(*src), self.load_xmm(*dst));
        let sv = self.bitcast_v(xs, types::I64X2);
        let s = self.builder.ins().extractlane(sv, *src_high as u8);
        let dv = self.bitcast_v(xd, types::I64X2);
        let r = self.builder.ins().insertlane(dv, s, *dst_high as u8);
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_load_half(&mut self, dst: &u8, addr: &Val, high: &bool) -> bool {
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

    pub(crate) fn emit_v_store_half(&mut self, addr: &Val, src: &u8, high: &bool) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, 8, 1);
        let xs = self.load_xmm(*src);
        let sv = self.bitcast_v(xs, types::I64X2);
        let s = self.builder.ins().extractlane(sv, *high as u8);
        self.gstore(s, host, 0);
        false
    }

    pub(crate) fn emit_v_extract_w(&mut self, dst: &u32, src: &u8, index: &u8) -> bool {
        let x = self.load_xmm(*src);
        let vec = self.bitcast_v(x, types::I16X8);
        let w = self.builder.ins().extractlane(vec, *index & 7);
        let r = self.builder.ins().uextend(types::I64, w);
        self.set(*dst, r);
        false
    }

    pub(crate) fn emit_v_extract_lane(
        &mut self,
        dst: &u32,
        src: &u8,
        index: &u8,
        size: &u8,
    ) -> bool {
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

    pub(crate) fn emit_v_move_mask_b(&mut self, dst: &u32, src: &u8) -> bool {
        let x = self.load_xmm(*src);
        let v = self.bitcast_v(x, types::I8X16);
        let mask = self.builder.ins().vhigh_bits(types::I32, v);
        let r = self.builder.ins().uextend(types::I64, mask);
        self.set(*dst, r);
        false
    }

    pub(crate) fn emit_v_zero_upper(&mut self, reg: &u8) -> bool {
        self.store_ymm_hi_zero(*reg);
        false
    }

    pub(crate) fn emit_v_zero_upper_all(&mut self) -> bool {
        for r in 0..16u8 {
            self.store_ymm_hi_zero(r);
        }
        false
    }

    pub(crate) fn emit_v_pshufb(&mut self, dst: &u8, a: &u8, idx: &u8) -> bool {
        let (xa, xi) = (self.load_xmm(*a), self.load_xmm(*idx));
        let r = self.emit_pshufb(xa, xi);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_pshufb_m(&mut self, dst: &u8, addr: &Val) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, 16, 0);
        let iv = self.gload(types::I128, host, 0);
        let xd = self.load_xmm(*dst);
        let r = self.emit_pshufb(xd, iv);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_alignr(&mut self, dst: &u8, a: &u8, src: &u8, imm: &u8) -> bool {
        let (xa, xs) = (self.load_xmm(*a), self.load_xmm(*src));
        let r = self.emit_palignr(xa, xs, *imm);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_alignr_m(&mut self, dst: &u8, addr: &Val, imm: &u8) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, 16, 0);
        let sv = self.gload(types::I128, host, 0);
        let xd = self.load_xmm(*dst);
        let r = self.emit_palignr(xd, sv, *imm);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_shufps(&mut self, dst: &u8, a: &u8, b: &u8, imm: &u8) -> bool {
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

    pub(crate) fn emit_v_shuffle16(&mut self, dst: &u8, a: &u8, imm: &u8, high: &bool) -> bool {
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

    pub(crate) fn emit_v_unpack_low(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        lane: &u8,
        high: &bool,
    ) -> bool {
        let mask = unpack_low_mask(*lane, *high);
        let (xa, xb) = (self.load_xmm(*a), self.load_xmm(*b));
        let va = self.bitcast_v(xa, types::I8X16);
        let vb = self.bitcast_v(xb, types::I8X16);
        let r = self.shuffle(va, vb, mask);
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_pack_us_w_b(&mut self, dst: &u8, a: &u8, b: &u8) -> bool {
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

    pub(crate) fn emit_v_insert_w(&mut self, dst: &u8, src: &Val, index: &u8) -> bool {
        let x = self.load_xmm(*dst);
        let vec = self.bitcast_v(x, types::I16X8);
        let val = self.val(*src);
        let v16 = self.builder.ins().ireduce(types::I16, val);
        let r = self.builder.ins().insertlane(vec, v16, *index & 7);
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_insert_lane(
        &mut self,
        dst: &u8,
        base: &u8,
        src: &Val,
        index: &u8,
        size: &u8,
    ) -> bool {
        let vty = match size {
            1 => types::I8X16,
            4 => types::I32X4,
            _ => types::I64X2,
        };
        let x = self.load_xmm(*base);
        let vec = self.bitcast_v(x, vty);
        let val = self.val(*src);
        let ev = self.narrow(val, *size);
        let lanes = 16 / *size;
        let r = self.builder.ins().insertlane(vec, ev, *index % lanes);
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_float_mov(&mut self, dst: &u8, a: &u8, src: &u8, prec: &FPrec) -> bool {
        // Merge the low lane preserving `a`'s upper bytes (integer lane insert).
        let lty = lane_int_vec_ty(*prec);
        let (xa, xs) = (self.load_xmm(*a), self.load_xmm(*src));
        let dv = self.bitcast_v(xa, lty);
        let sv = self.bitcast_v(xs, lty);
        let s0 = self.builder.ins().extractlane(sv, 0);
        let r = self.builder.ins().insertlane(dv, s0, 0);
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_float_bin(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        op: &FloatBinOp,
        prec: &FPrec,
        scalar: &bool,
    ) -> bool {
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

    pub(crate) fn emit_v_float_bin_m(
        &mut self,
        dst: &u8,
        addr: &Val,
        op: &FloatBinOp,
        prec: &FPrec,
        scalar: &bool,
    ) -> bool {
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

    pub(crate) fn emit_v_float_cmp(&mut self, a: &Val, b: &Val, prec: &FPrec) -> bool {
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

    pub(crate) fn emit_v_float_cmp_mask(
        &mut self,
        dst: &u8,
        a: &u8,
        b: &u8,
        prec: &FPrec,
        scalar: &bool,
        pred: &u8,
    ) -> bool {
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

    pub(crate) fn emit_v_cvt_from_int(
        &mut self,
        dst: &u8,
        src: &Val,
        int_size: &u8,
        prec: &FPrec,
        signed: &bool,
    ) -> bool {
        let raw = self.val(*src);
        let f = if *signed {
            let sv = self.sign_extend(raw, *int_size);
            self.builder.ins().fcvt_from_sint(scalar_fty(*prec), sv)
        } else {
            // Zero-extend the low `int_size` bytes, then unsigned convert (task-195).
            let uv = if *int_size == 8 {
                raw
            } else {
                self.builder.ins().band_imm(raw, 0xffff_ffff)
            };
            self.builder.ins().fcvt_from_uint(scalar_fty(*prec), uv)
        };
        let fbits = self.bitcast_scalar(lane_int_ty(*prec), f);
        let xd = self.load_xmm(*dst);
        let dv = self.bitcast_v(xd, lane_int_vec_ty(*prec));
        let r = self.builder.ins().insertlane(dv, fbits, 0);
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }

    pub(crate) fn emit_v_cvt_to_int(
        &mut self,
        dst: &u32,
        src: &Val,
        int_size: &u8,
        prec: &FPrec,
        trunc: &bool,
        signed: &bool,
    ) -> bool {
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
        // clamp out-of-range to the destination's MIN/MAX; the x86
        // integer-indefinite result on invalid operands is deferred). `signed`
        // picks `*2si` vs the AVX-512 unsigned `*2usi` form (task-195).
        let ity = if *int_size == 8 {
            types::I64
        } else {
            types::I32
        };
        let iv = if *signed {
            self.builder.ins().fcvt_to_sint_sat(ity, f)
        } else {
            self.builder.ins().fcvt_to_uint_sat(ity, f)
        };
        let iv64 = if *int_size == 8 {
            iv
        } else {
            self.builder.ins().uextend(types::I64, iv)
        };
        self.set(*dst, iv64);
        false
    }

    pub(crate) fn emit_v_cvt_float(
        &mut self,
        dst: &u8,
        src: &Val,
        from: &FPrec,
        to: &FPrec,
    ) -> bool {
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

    pub(crate) fn emit_v_float_unary(
        &mut self,
        dst: &u8,
        a: &u8,
        src: &u8,
        op: &FloatUnOp,
        prec: &FPrec,
        scalar: &bool,
    ) -> bool {
        let fty = float_vec_ty(*prec);
        let xs = self.load_xmm(*src);
        let vs = self.bitcast_v(xs, fty);
        let r = if *scalar {
            let s0 = self.builder.ins().extractlane(vs, 0);
            let z = self.emit_funary(s0, *op);
            // Preserve the merge base's upper lane(s).
            let xa = self.load_xmm(*a);
            let va = self.bitcast_v(xa, fty);
            self.builder.ins().insertlane(va, z, 0)
        } else {
            self.emit_funary(vs, *op)
        };
        let r = self.bitcast_i128(r);
        self.store_xmm(*dst, r);
        false
    }
}
