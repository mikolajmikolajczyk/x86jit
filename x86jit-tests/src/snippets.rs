//! Guest snippets the differential and JIT suites both run.
//!
//! Each of these assembles one guest program exercising a slice of the ISA. They are
//! here rather than in a test binary because **two** binaries run them and must run the
//! *same* bytes: `differential.rs` checks the interpreter against an independent oracle,
//! `jit.rs` checks the JIT against the interpreter. A snippet fixed in one copy and not
//! the other would leave the two legs silently validating different programs — which is
//! exactly what a second copy invites and what nothing would have caught.
//!
//! Each body takes a `CodeAssembler` and appends to it, so a caller decides the memory
//! layout, the entry point and how the result is compared.

use iced_x86::code_asm::*;

/// Scratch page the snippets load from and store to. A caller must map it writable.
pub const SCRATCH: u64 = 0x8000;

/// Scalar SSE2 double: cvtsi2sd/movsd/add/sub/mul/div, a memory source, both
/// convert-to-int roundings, precision converts, and a compare setting flags. All
/// values are exact IEEE doubles so the result is bit-stable against the CPU.
pub fn float_scalar_body(a: &mut CodeAssembler) {
    a.mov(rax, 7i64).unwrap();
    a.cvtsi2sd(xmm0, rax).unwrap(); // 7.0
    a.mov(rax, 2i64).unwrap();
    a.cvtsi2sd(xmm1, rax).unwrap(); // 2.0
    a.movsd_2(xmm2, xmm0).unwrap(); // 7.0 (reg merge)
    a.addsd(xmm2, xmm1).unwrap(); // 9.0
    a.subsd(xmm2, xmm0).unwrap(); // 2.0
    a.mulsd(xmm2, xmm0).unwrap(); // 14.0
    a.divsd(xmm2, xmm1).unwrap(); // 7.0
    a.mov(rax, 0x4008_0000_0000_0000u64).unwrap(); // 3.0
    a.mov(qword_ptr(SCRATCH), rax).unwrap();
    a.addsd(xmm2, qword_ptr(SCRATCH)).unwrap(); // 10.0 (mem source)
    a.cvttsd2si(rcx, xmm2).unwrap(); // 10
                                     // 3.5 -> trunc 3, round-half-to-even 4.
    a.mov(rax, 7i64).unwrap();
    a.cvtsi2sd(xmm3, rax).unwrap();
    a.divsd(xmm3, xmm1).unwrap(); // 3.5
    a.cvttsd2si(rdx, xmm3).unwrap(); // 3
    a.cvtsd2si(rsi, xmm3).unwrap(); // 4
    a.mov(rax, -5i64).unwrap();
    a.cvtsi2sd(xmm4, rax).unwrap(); // -5.0
    a.cvttsd2si(rdi, xmm4).unwrap(); // -5
    a.cvtsd2ss(xmm5, xmm2).unwrap(); // 10.0 -> f32
    a.cvtss2sd(xmm6, xmm5).unwrap(); // -> f64
    a.ucomisd(xmm0, xmm1).unwrap(); // 7 vs 2: CF=0 ZF=0 PF=0
    a.hlt().unwrap();
}

/// Packed double (mulpd/addpd/subpd + a memory source) and packed single
/// (mulps/addps/divps), plus scalar single and a `comiss` compare.
pub fn float_packed_body(a: &mut CodeAssembler) {
    // packed double [1.5, 2.5]
    a.mov(rax, 0x3FF8_0000_0000_0000u64).unwrap(); // 1.5
    a.movq(xmm0, rax).unwrap();
    a.mov(rax, 0x4004_0000_0000_0000u64).unwrap(); // 2.5
    a.movq(xmm1, rax).unwrap();
    a.punpcklqdq(xmm0, xmm1).unwrap(); // [1.5, 2.5]
    a.movapd(xmm2, xmm0).unwrap();
    a.mulpd(xmm2, xmm0).unwrap(); // [2.25, 6.25]
    a.addpd(xmm2, xmm0).unwrap(); // [3.75, 8.75]
    a.subpd(xmm2, xmm0).unwrap(); // [2.25, 6.25]
    a.movupd(xmmword_ptr(SCRATCH), xmm0).unwrap();
    a.mulpd(xmm2, xmmword_ptr(SCRATCH)).unwrap(); // [3.375, 15.625] (mem source)
                                                  // packed single [1,2,3,4]
    a.mov(rax, 0x4000_0000_3F80_0000u64).unwrap(); // 1.0, 2.0
    a.movq(xmm3, rax).unwrap();
    a.mov(rax, 0x4080_0000_4040_0000u64).unwrap(); // 3.0, 4.0
    a.movq(xmm4, rax).unwrap();
    a.punpcklqdq(xmm3, xmm4).unwrap(); // [1,2,3,4]
    a.mulps(xmm3, xmm3).unwrap(); // [1,4,9,16]
    a.addps(xmm3, xmm3).unwrap(); // [2,8,18,32]
    a.divps(xmm3, xmm3).unwrap(); // [1,1,1,1]
                                  // scalar single
    a.mov(rax, 9i64).unwrap();
    a.cvtsi2ss(xmm5, rax).unwrap(); // 9.0f
    a.mov(rax, 4i64).unwrap();
    a.cvtsi2ss(xmm6, rax).unwrap(); // 4.0f
    a.movss(xmm7, xmm5).unwrap();
    a.addss(xmm7, xmm6).unwrap(); // 13.0
    a.mulss(xmm7, xmm6).unwrap(); // 52.0
    a.subss(xmm7, xmm6).unwrap(); // 48.0
    a.divss(xmm7, xmm6).unwrap(); // 12.0
    a.cvttss2si(r10, xmm7).unwrap(); // 12
    a.comiss(xmm5, xmm6).unwrap(); // 9 vs 4: CF=0 ZF=0 PF=0
                                   // min/max (scalar + packed) and sqrt
    a.minsd(xmm2, xmm0).unwrap();
    a.maxpd(xmm0, xmm1).unwrap();
    a.minps(xmm3, xmm4).unwrap();
    a.maxss(xmm5, xmm6).unwrap();
    a.mov(rax, 16i64).unwrap();
    a.cvtsi2sd(xmm8, rax).unwrap(); // 16.0
    a.sqrtsd(xmm9, xmm8).unwrap(); // 4.0
    a.sqrtss(xmm10, xmm5).unwrap(); // sqrt(9) = 3
    a.xorpd(xmm11, xmm11).unwrap(); // zero via pd-logic alias
    a.hlt().unwrap();
}

/// x87 stack arithmetic, int/float load-store, fchs/fabs, and a compare.
pub fn x87_body(a: &mut CodeAssembler) {
    a.mov(rax, 0x4008_0000_0000_0000u64).unwrap(); // 3.0
    a.mov(qword_ptr(SCRATCH), rax).unwrap();
    a.mov(rax, 0x4010_0000_0000_0000u64).unwrap(); // 4.0
    a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
    a.fld(qword_ptr(SCRATCH)).unwrap();
    a.fld(qword_ptr(SCRATCH + 8)).unwrap();
    a.faddp(st1, st0).unwrap(); // 7
    a.fld1().unwrap();
    a.fld1().unwrap();
    a.faddp(st1, st0).unwrap(); // 2
    a.fmulp(st1, st0).unwrap(); // 14
    a.fld1().unwrap();
    a.fsubp(st1, st0).unwrap(); // 13
    a.fst(qword_ptr(SCRATCH + 16)).unwrap();
    a.fistp(qword_ptr(SCRATCH + 24)).unwrap();
    a.mov(r8, qword_ptr(SCRATCH + 16)).unwrap();
    a.mov(r9, qword_ptr(SCRATCH + 24)).unwrap();
    a.mov(dword_ptr(SCRATCH + 32), 5i32).unwrap();
    a.fild(dword_ptr(SCRATCH + 32)).unwrap();
    a.fchs().unwrap();
    a.fabs().unwrap();
    a.fistp(dword_ptr(SCRATCH + 36)).unwrap();
    a.mov(r10d, dword_ptr(SCRATCH + 36)).unwrap();
    a.fld1().unwrap();
    a.fldz().unwrap();
    a.fucomip(st0, st1).unwrap();
    a.setb(r11b).unwrap();
    a.hlt().unwrap();
}

/// The round trip FreeBSD's `<fenv.h>` performs around `powf`/`expf`, which is where
/// a real guest faulted: `fnstenv` out, patch the saved control word, `fldenv`
/// back. **No `fldcw` appears anywhere in this body** — the only path from the patched
/// image back into the FPU is `fldenv` itself, so a `fldenv` that parsed the control
/// word and discarded it would leave every probe rounding to nearest.
///
/// Each of the four rounding-control encodings is written into the saved image and
/// restored, then applied to 0.75 and -0.75 by `fistp word` (the one operation whose
/// result the control word's RC field steers here). The pair separates all four modes:
/// nearest `(1, -1)`, down `(0, -1)`, up `(1, 0)`, truncate `(0, 0)`.
///
/// The tail probes the other modeled field: the status word's TOP, patched to 5 and
/// read back with `fnstsw ax`.
pub fn x87_fldenv_body(a: &mut CodeAssembler) {
    a.mov(rax, SCRATCH).unwrap();
    for (off, bits) in [
        (0x100u64, 0x3FE8_0000_0000_0000u64), // 0.75
        (0x108, 0xBFE8_0000_0000_0000),       // -0.75
    ] {
        a.mov(rcx, bits).unwrap();
        a.mov(qword_ptr(rax + off), rcx).unwrap();
    }
    a.fninit().unwrap();
    a.fnstenv(ptr(rax)).unwrap();

    for rc in 0..4u64 {
        // Patch only the control word of the saved image: 0x037F is what `fninit`
        // leaves (round-to-nearest, all masks set), RC is bits 11:10.
        a.mov(cx, (0x037F | (rc << 10)) as i32).unwrap();
        a.mov(word_ptr(rax), cx).unwrap();
        a.fldenv(ptr(rax)).unwrap();
        a.fld(qword_ptr(rax + 0x100u64)).unwrap();
        a.fistp(word_ptr(rax + (0x180 + rc * 4))).unwrap();
        a.fld(qword_ptr(rax + 0x108u64)).unwrap();
        a.fistp(word_ptr(rax + (0x182 + rc * 4))).unwrap();
    }
    a.fnstcw(word_ptr(rax + 0x60u64)).unwrap();

    // TOP is the only status-word field this FPU models, so it is the only one the
    // image can round-trip: patch it to 5 and let `fnstsw` report it back.
    a.mov(cx, 0x2800i32).unwrap();
    a.mov(word_ptr(rax + 4u64), cx).unwrap();
    a.fldenv(ptr(rax)).unwrap();
    // `fnstsw ax` clobbers the low half of RAX, so the store back uses an absolute
    // address rather than the base register it just destroyed.
    a.fnstsw(ax).unwrap();
    a.mov(word_ptr(SCRATCH + 0x1A0), ax).unwrap();
    a.hlt().unwrap();
}

/// shufps/shufpd, cmpltsd, psraw/psrad, punpckh*, and pshufd with a memory source.
pub fn sse_shuffle_cmp_body(a: &mut CodeAssembler) {
    a.mov(rax, 0x0706_0504_0302_0100u64).unwrap();
    a.movq(xmm0, rax).unwrap();
    a.mov(rax, 0x0F0E_0D0C_0B0A_0908u64).unwrap();
    a.movq(xmm1, rax).unwrap();
    a.punpcklqdq(xmm0, xmm1).unwrap();
    a.movdqa(xmm2, xmm0).unwrap();
    a.shufps(xmm2, xmm0, 0x1B).unwrap();
    a.movq(r8, xmm2).unwrap();
    a.movdqa(xmm3, xmm0).unwrap();
    a.shufpd(xmm3, xmm0, 0x1).unwrap();
    a.movq(r9, xmm3).unwrap();
    a.movdqa(xmm4, xmm0).unwrap();
    a.punpckhbw(xmm4, xmm1).unwrap();
    a.movq(r10, xmm4).unwrap();
    a.movdqa(xmm5, xmm0).unwrap();
    a.punpckhwd(xmm5, xmm1).unwrap();
    a.movq(r11, xmm5).unwrap();
    a.movdqa(xmm6, xmm0).unwrap();
    a.punpckhdq(xmm6, xmm1).unwrap();
    a.movq(r12, xmm6).unwrap();
    a.mov(rax, 0x8000_4000_FF00_0100u64).unwrap();
    a.movq(xmm7, rax).unwrap();
    a.movdqa(xmm8, xmm7).unwrap();
    a.psraw(xmm8, 4).unwrap();
    a.movq(r13, xmm8).unwrap();
    a.movdqa(xmm9, xmm7).unwrap();
    a.psrad(xmm9, 20).unwrap();
    a.movq(r14, xmm9).unwrap();
    a.mov(rax, 3i64).unwrap();
    a.cvtsi2sd(xmm10, rax).unwrap();
    a.mov(rax, 5i64).unwrap();
    a.cvtsi2sd(xmm11, rax).unwrap();
    a.cmpltsd(xmm10, xmm11).unwrap();
    a.movq(r15, xmm10).unwrap();
    a.movdqu(xmmword_ptr(SCRATCH), xmm0).unwrap();
    a.pshufd(xmm12, xmmword_ptr(SCRATCH), 0x1B).unwrap();
    a.movq(rbx, xmm12).unwrap();
    a.hlt().unwrap();
}

/// pmovmskb, packed unsigned/signed min/max, pcmpgt, movlpd/movhpd.
pub fn sse_string_body(a: &mut CodeAssembler) {
    a.mov(rax, 0x8000_7F01_0080_00FFu64).unwrap();
    a.movq(xmm0, rax).unwrap();
    a.mov(rax, 0x0102_8304_0586_0708u64).unwrap();
    a.movq(xmm1, rax).unwrap();
    a.punpcklqdq(xmm0, xmm1).unwrap();
    a.pmovmskb(ecx, xmm0).unwrap();
    a.mov(rax, 0x1020_3040_5060_7080u64).unwrap();
    a.movq(xmm2, rax).unwrap();
    a.mov(rax, 0x151F_353F_555F_757Fu64).unwrap();
    a.movq(xmm3, rax).unwrap();
    a.movdqa(xmm4, xmm2).unwrap();
    a.pminub(xmm4, xmm3).unwrap();
    a.movq(r8, xmm4).unwrap();
    a.movdqa(xmm5, xmm2).unwrap();
    a.pmaxub(xmm5, xmm3).unwrap();
    a.movq(r9, xmm5).unwrap();
    a.movdqa(xmm6, xmm2).unwrap();
    a.pminsw(xmm6, xmm3).unwrap();
    a.movq(r10, xmm6).unwrap();
    a.movdqa(xmm7, xmm2).unwrap();
    a.pmaxsw(xmm7, xmm3).unwrap();
    a.movq(r11, xmm7).unwrap();
    a.movdqa(xmm8, xmm2).unwrap();
    a.pcmpgtb(xmm8, xmm3).unwrap();
    a.movq(r12, xmm8).unwrap();
    a.movdqa(xmm9, xmm2).unwrap();
    a.pcmpgtd(xmm9, xmm3).unwrap();
    a.movq(r13, xmm9).unwrap();
    a.movdqu(xmmword_ptr(SCRATCH), xmm0).unwrap();
    a.movhpd(xmm10, qword_ptr(SCRATCH)).unwrap();
    a.movq(r14, xmm10).unwrap();
    a.pshufd(xmm10, xmm10, 0x4E).unwrap();
    a.movq(r15, xmm10).unwrap();
    a.hlt().unwrap();
}

/// cwd/cdq/cqo sign-extension and bsf/bsr (incl. src==0 → ZF, dest preserved).
pub fn bitscan_cdq_body(a: &mut CodeAssembler) {
    a.mov(eax, 0x8000_0000u32 as i32).unwrap();
    a.cdq().unwrap();
    a.mov(r8d, edx).unwrap();
    a.mov(eax, 0x4000_0000i32).unwrap();
    a.cdq().unwrap();
    a.mov(r9d, edx).unwrap();
    a.mov(eax, 0x0000_0100i32).unwrap();
    a.bsf(ebx, eax).unwrap();
    a.bsr(r10d, eax).unwrap();
    a.mov(rax, 0x8000_0000_0000_0000u64).unwrap();
    a.bsr(r11, rax).unwrap();
    a.bsf(r12, rax).unwrap();
    a.mov(r13, 0xDEADu64).unwrap();
    a.mov(esi, 0i32).unwrap();
    a.bsf(r13d, esi).unwrap();
    a.setz(r14b).unwrap();
    a.mov(eax, 1i32).unwrap();
    a.bsf(ebp, eax).unwrap();
    a.setz(r15b).unwrap();
    a.hlt().unwrap();
}

/// pshuflw/pshufhw, pextrw, movlhps/movhlps, movhps/movlps (mem load + store).
pub fn sse_half_body(a: &mut CodeAssembler) {
    a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
    a.movq(xmm0, rax).unwrap();
    a.mov(rax, 0x99AA_BBCC_DDEE_FF00u64).unwrap();
    a.movq(xmm1, rax).unwrap();
    a.punpcklqdq(xmm0, xmm1).unwrap();
    a.pshuflw(xmm2, xmm0, 0x1Bi32).unwrap();
    a.pshufhw(xmm3, xmm0, 0x1Bi32).unwrap();
    a.pextrw(ecx, xmm0, 3i32).unwrap();
    a.movlhps(xmm4, xmm0).unwrap();
    a.movhlps(xmm5, xmm0).unwrap();
    a.movdqu(xmmword_ptr(SCRATCH), xmm0).unwrap();
    a.movhps(xmm6, qword_ptr(SCRATCH)).unwrap();
    a.movlps(xmm7, qword_ptr(SCRATCH + 8)).unwrap();
    a.movhps(qword_ptr(SCRATCH + 16), xmm0).unwrap();
    a.movlps(qword_ptr(SCRATCH + 32), xmm0).unwrap();
    a.mov(r8, qword_ptr(SCRATCH + 16)).unwrap();
    a.mov(r9, qword_ptr(SCRATCH + 32)).unwrap();
    a.hlt().unwrap();
}

/// bt/bts/btr/btc with register and immediate indices, register and memory
/// operands; CF captured per-op via `setb`, writebacks read into registers.
pub fn bit_test_body(a: &mut CodeAssembler) {
    a.mov(rax, 0xAi64).unwrap();
    a.bt(rax, 3i32).unwrap();
    a.setb(r8b).unwrap();
    a.bt(rax, 2i32).unwrap();
    a.setb(r9b).unwrap();
    a.mov(rcx, 1i64).unwrap();
    a.bt(rax, rcx).unwrap();
    a.setb(r10b).unwrap();
    a.bts(rax, 0i32).unwrap();
    a.setb(r11b).unwrap();
    a.mov(rdx, rax).unwrap();
    a.btr(rax, 1i32).unwrap();
    a.setb(r12b).unwrap();
    a.mov(rsi, rax).unwrap();
    a.btc(rax, 2i32).unwrap();
    a.setb(r13b).unwrap();
    a.mov(rdi, rax).unwrap();
    a.mov(qword_ptr(SCRATCH), 0xF0i32).unwrap();
    a.bt(qword_ptr(SCRATCH), 5i32).unwrap();
    a.setb(r14b).unwrap();
    a.bts(qword_ptr(SCRATCH), 0i32).unwrap();
    a.mov(r15, qword_ptr(SCRATCH)).unwrap();
    a.hlt().unwrap();
}

/// Locked RMW, xchg, xadd, and cmpxchg (success + failure) across byte/dword/qword
/// sizes, matched bit-for-bit against the real CPU (values + flags). Memory
/// effects are read back into registers so the snapshot observes them.
pub fn atomics_body(a: &mut CodeAssembler) {
    a.mov(qword_ptr(SCRATCH), 100i32).unwrap();
    a.mov(rax, 5i64).unwrap();
    a.lock().add(qword_ptr(SCRATCH), rax).unwrap(); // mem = 105
    a.mov(rbx, 3i64).unwrap();
    a.lock().xadd(qword_ptr(SCRATCH), rbx).unwrap(); // rbx = 105 (old), mem = 108
    a.mov(r8, qword_ptr(SCRATCH)).unwrap(); // r8 = 108
    a.lock().inc(qword_ptr(SCRATCH)).unwrap(); // mem = 109
    a.lock().dec(qword_ptr(SCRATCH)).unwrap(); // mem = 108
    a.mov(r9, qword_ptr(SCRATCH)).unwrap(); // r9 = 108
    a.mov(r10, 777i64).unwrap();
    a.xchg(qword_ptr(SCRATCH), r10).unwrap(); // r10 = 108 (old), mem = 777
    a.mov(r11, qword_ptr(SCRATCH)).unwrap(); // r11 = 777
    a.mov(dword_ptr(SCRATCH + 16), 0xF0i32).unwrap();
    a.mov(ecx, 0x0Fi32).unwrap();
    a.lock().or(dword_ptr(SCRATCH + 16), ecx).unwrap(); // mem32 = 0xFF
    a.mov(r14d, dword_ptr(SCRATCH + 16)).unwrap();
    a.mov(qword_ptr(SCRATCH), 42i32).unwrap();
    a.mov(rax, 42i64).unwrap();
    a.mov(rsi, 99i64).unwrap();
    a.lock().cmpxchg(qword_ptr(SCRATCH), rsi).unwrap(); // match: mem = 99, ZF = 1, rax = 42
    a.mov(r12, qword_ptr(SCRATCH)).unwrap(); // r12 = 99
    a.mov(byte_ptr(SCRATCH + 24), 1i32).unwrap();
    a.lock().add(byte_ptr(SCRATCH + 24), al).unwrap(); // 1 + 42 = 43
    a.movzx(r15, byte_ptr(SCRATCH + 24)).unwrap(); // r15 = 43
    a.mov(rax, 7i64).unwrap();
    a.mov(rdi, 123i64).unwrap();
    a.lock().cmpxchg(qword_ptr(SCRATCH), rdi).unwrap(); // mismatch: rax = 99, ZF = 0
    a.mov(r13, qword_ptr(SCRATCH)).unwrap(); // r13 = 99 (unchanged)
    a.hlt().unwrap();
}
