use super::*;

impl Translator<'_, '_> {
    pub(crate) fn emit_insn_start(&mut self, guest_addr: &u64) -> bool {
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

    pub(crate) fn emit_read_reg(&mut self, dst: &u32, reg: &Reg) -> bool {
        let v = self.read_reg(*reg);
        self.set(*dst, v);
        false
    }

    pub(crate) fn emit_write_reg(&mut self, reg: &Reg, src: &Val, size: &u8) -> bool {
        let v = self.val(*src);
        self.write_reg(*reg, v, *size);
        false
    }

    pub(crate) fn emit_get_cond(&mut self, dst: &u32, cond: &Cond) -> bool {
        let c = self.eval_cond(*cond);
        let v = self.builder.ins().uextend(types::I64, c);
        self.set(*dst, v);
        false
    }

    pub(crate) fn emit_set_df(&mut self, value: &bool) -> bool {
        let v = self.builder.ins().iconst(types::I8, *value as i64);
        self.store_flag(self.offsets.df, v);
        false
    }

    pub(crate) fn emit_jump(&mut self, target: &Val) -> bool {
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

    pub(crate) fn emit_branch(&mut self, cond: &Cond, taken: &u64, fallthrough: &u64) -> bool {
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

    pub(crate) fn emit_call(
        &mut self,
        target: &Val,
        return_addr: &u64,
        slot: &u8,
        wrap_sp: &bool,
    ) -> bool {
        let rsp = self.read_gpr(RSP);
        let delta = self.iconst(*slot as u64);
        let mut newsp = self.builder.ins().isub(rsp, delta);
        if *wrap_sp {
            newsp = self.builder.ins().band_imm(newsp, 0xffff_ffff);
        }
        let host = self.checked_addr(newsp, *slot, 1);
        let ra = self.iconst(*return_addr);
        self.store_guest(host, ra, *slot);
        self.write_gpr(RSP, newsp, if *wrap_sp { 4 } else { 8 });
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

    pub(crate) fn emit_ret(&mut self, slot: &u8, pop_extra: &u16, wrap_sp: &bool) -> bool {
        let rsp = self.read_gpr(RSP);
        let host = self.checked_addr(rsp, *slot, 0);
        let ret = self.load_guest(host, *slot);
        let delta = self.iconst(*slot as u64 + *pop_extra as u64);
        let mut newsp = self.builder.ins().iadd(rsp, delta);
        if *wrap_sp {
            newsp = self.builder.ins().band_imm(newsp, 0xffff_ffff);
        }
        self.write_gpr(RSP, newsp, if *wrap_sp { 4 } else { 8 });
        self.store_cpu(self.offsets.rip, ret);
        // Return prediction (R5): pop the shadow ring and chain to the
        // caller's continuation if the predicted address matches the actual
        // popped target; otherwise fall back to dispatch.
        self.emit_ret_predict(ret);
        true
    }

    pub(crate) fn emit_syscall(&mut self) -> bool {
        let end = self.iconst(self.guest_end);
        self.store_cpu(self.offsets.rip, end);
        self.ret(RET_SYSCALL);
        true
    }

    pub(crate) fn emit_hlt(&mut self) -> bool {
        let end = self.iconst(self.guest_end);
        self.store_cpu(self.offsets.rip, end);
        self.ret(RET_HLT);
        true
    }

    pub(crate) fn emit_trap(&mut self, vector: &u8, advance: &u8) -> bool {
        // A lifted architectural exception. x86 saved-RIP: fault (advance 0)
        // stays on the instruction, trap (advance = length) resumes past it —
        // matching the interpreter. The vector goes to the MemCtx out-field the
        // dispatcher reads (which sets `Exit::Exception.addr` from this RIP).
        let rip = self.iconst(self.cur_addr + *advance as u64);
        self.store_cpu(self.offsets.rip, rip);
        let vec = self.iconst(*vector as u64);
        self.store_mem(MEMCTX_EXCEPTION_VECTOR, vec);
        self.ret(RET_EXCEPTION);
        true
    }

    pub(crate) fn emit_port_io(&mut self) -> bool {
        // Port I/O (§5.2) — deferred to the interpreter, exactly like an
        // inlined MMIO access: set RIP to the instruction and hand back
        // `RET_PORTIO_DEFER`. The dispatcher single-steps it on the interp,
        // which produces `Exit::PortIo` (and, for `in`, records the pending
        // accumulator width). Rare and self-contained, so no need to open-code
        // the accumulator read / out-field plumbing in the backend.
        let rip = self.iconst(self.cur_addr);
        self.store_cpu(self.offsets.rip, rip);
        self.ret(RET_PORTIO_DEFER);
        true
    }
}
