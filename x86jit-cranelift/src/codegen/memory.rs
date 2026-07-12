use super::*;

impl Translator<'_, '_> {
    pub(crate) fn emit_load(&mut self, dst: &u32, addr: &Val, size: &u8) -> bool {
        let a = self.val(*addr);
        let host = self.checked_addr(a, *size, 0);
        let v = self.load_guest(host, *size);
        self.set(*dst, v);
        false
    }

    pub(crate) fn emit_store(&mut self, addr: &Val, src: &Val, size: &u8) -> bool {
        let a = self.val(*addr);
        let v = self.val(*src);
        let host = self.checked_addr(a, *size, 1);
        self.store_guest(host, v, *size);
        self.note_watched_store(a, *size);
        false
    }

    pub(crate) fn emit_atomic_rmw(
        &mut self,
        old: &u32,
        addr: &Val,
        src: &Val,
        size: &u8,
        op: &RmwOp,
    ) -> bool {
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
        self.note_watched_store(a, *size);
        false
    }

    pub(crate) fn emit_atomic_cas(
        &mut self,
        old: &u32,
        addr: &Val,
        expected: &Val,
        src: &Val,
        size: &u8,
    ) -> bool {
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
        self.note_watched_store(a, *size);
        false
    }

    pub(crate) fn emit_rep_string(
        &mut self,
        op: &StrOp,
        elem: &u8,
        rep: &RepKind,
        addr_bits: &u8,
        seg_base: &Val,
    ) -> bool {
        let op_code = self.iconst(str_op_code(*op));
        let elem = self.iconst(*elem as u64);
        let rep = self.iconst(rep_code(*rep));
        let cur = self.iconst(self.cur_addr);
        let abits = self.iconst(*addr_bits as u64);
        // Resolve the DS-source segment base (0, or an FS/GS-base temp) BEFORE the
        // GPR flush; the shared `string_run` adds it to the RSI-relative reads.
        let seg = self.val(*seg_base);
        let args = [self.cpu, self.mem, op_code, elem, rep, cur, abits, seg];
        self.flush_gprs(); // helper reads/advances RSI/RDI/RCX in CpuState
        let inst = self.call_helper(self.helpers.string, &args);
        self.trap_if_unmapped(inst);
        self.reload_gprs(); // helper advanced RSI/RDI/RCX
        false
    }
}
