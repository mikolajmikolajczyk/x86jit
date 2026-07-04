//! Differential acceptance (M1, testing.md §11): for each snippet the
//! interpreter's final state must equal Unicorn's. Gated behind the `unicorn`
//! feature (the native oracle). Run with:
//!
//! ```text
//! cargo nextest run -p x86jit-tests --features unicorn
//! ```

#![cfg(feature = "unicorn")]

use iced_x86::code_asm::*;
use x86jit_tests::builder::Vector;
use x86jit_tests::vector::{CpuSnapshot, FlagName};

/// Base of the builder's auto-mapped scratch RW page (data + stack).
const SCRATCH: u64 = 0x8000;

/// Assemble a snippet, run it through the interpreter and Unicorn, assert
/// identical final state (with the given undefined-flag mask).
fn diff(
    build: impl FnOnce(&mut CodeAssembler),
    init: impl FnOnce(&mut CpuSnapshot),
    dont_care: &[FlagName],
) {
    Vector::asm(build).init(init).dont_care(dont_care).assert_matches_unicorn();
}

#[test]
fn mov_r32_zeroes_upper() {
    diff(
        |a| {
            a.mov(rax, 0xFFFF_FFFF_FFFF_FFFFu64).unwrap();
            a.mov(eax, 5i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn add_carry_and_overflow() {
    diff(
        |a| {
            a.mov(eax, 0x7FFF_FFFFi32).unwrap();
            a.add(eax, 1i32).unwrap(); // signed overflow: OF=1, SF=1
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn add_unsigned_carry_out() {
    diff(
        |a| {
            a.mov(eax, -1i32).unwrap(); // 0xFFFFFFFF
            a.add(eax, 2i32).unwrap(); // CF=1, result 1
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn sub_borrow_sets_flags() {
    diff(
        |a| {
            a.mov(eax, 0i32).unwrap();
            a.sub(eax, 1i32).unwrap(); // CF=1, SF=1, result 0xFFFFFFFF
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn cmp_sets_flags_without_writeback() {
    diff(
        |a| {
            a.mov(rax, 42u64).unwrap();
            a.cmp(rax, 42i32).unwrap(); // ZF=1, rax unchanged
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn logic_forces_cf_of_zero() {
    // and/or/xor clear CF and OF; AF is architecturally undefined -> masked.
    diff(
        |a| {
            a.mov(eax, 0xF0F0i32).unwrap();
            a.and(eax, 0x0FF0i32).unwrap();
            a.or(eax, 0x0003i32).unwrap();
            a.xor(eax, 0x00FFi32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Af],
    );
}

#[test]
fn lea_base_index_scale_disp() {
    diff(
        |a| {
            a.mov(rbx, 0x10u64).unwrap();
            a.mov(rcx, 0x3u64).unwrap();
            a.lea(rax, qword_ptr(rbx + rcx * 4 + 8)).unwrap(); // 0x10 + 0xC + 8 = 0x24
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn store_then_load() {
    diff(
        |a| {
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rbx, qword_ptr(SCRATCH)).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn conditional_countdown_loop() {
    diff(
        |a| {
            let mut top = a.create_label();
            a.mov(ecx, 5i32).unwrap();
            a.mov(eax, 0i32).unwrap();
            a.set_label(&mut top).unwrap();
            a.add(eax, ecx).unwrap();
            a.sub(ecx, 1i32).unwrap();
            a.jnz(top).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn push_pop_roundtrip() {
    diff(
        |a| {
            a.mov(rax, 0xDEAD_BEEF_CAFE_B0BAu64).unwrap();
            a.push(rax).unwrap();
            a.pop(rbx).unwrap();
            a.hlt().unwrap();
        },
        |cpu| cpu.gpr[4] = Vector::scratch(), // rsp mid-scratch
        &[],
    );
}

#[test]
fn adc_carry_chain() {
    // 64-bit add that carries, then adc consumes CF (the 128-bit add pattern).
    diff(
        |a| {
            a.mov(rax, 0xFFFF_FFFF_FFFF_FFFFu64).unwrap();
            a.add(rax, 1i32).unwrap(); // CF=1, rax=0
            a.mov(rcx, 5u64).unwrap();
            a.adc(rcx, 0i32).unwrap(); // rcx = 5 + 0 + CF = 6
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn sbb_borrow_chain() {
    diff(
        |a| {
            a.mov(eax, 0i32).unwrap();
            a.sub(eax, 1i32).unwrap(); // CF=1
            a.mov(ecx, 10i32).unwrap();
            a.sbb(ecx, 3i32).unwrap(); // ecx = 10 - 3 - CF = 6
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn inc_dec_preserve_carry() {
    diff(
        |a| {
            a.mov(eax, 0i32).unwrap();
            a.sub(eax, 1i32).unwrap(); // CF=1
            a.mov(ecx, 41i32).unwrap();
            a.inc(ecx).unwrap(); // ecx=42, CF must stay 1
            a.dec(ecx).unwrap(); // ecx=41, CF still 1
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn neg_sets_carry_and_not_leaves_flags() {
    diff(
        |a| {
            a.mov(eax, 5i32).unwrap();
            a.neg(eax).unwrap(); // eax = -5, CF=1
            a.mov(ecx, 0x0F0Fi32).unwrap();
            a.not(ecx).unwrap(); // bitwise, no flag change
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn movzx_and_movsx() {
    diff(
        |a| {
            a.mov(ebx, 0x80i32).unwrap(); // bl = 0x80
            a.movzx(eax, bl).unwrap(); // 0x0000_0080
            a.movsx(ecx, bl).unwrap(); // 0xFFFF_FF80
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn movsxd_and_cdqe() {
    diff(
        |a| {
            a.mov(eax, -3i32).unwrap(); // eax = 0xFFFFFFFD, rax upper zeroed
            a.movsxd(rbx, eax).unwrap(); // rbx = 0xFFFFFFFFFFFFFFFD
            a.cdqe().unwrap(); // rax sign-extends eax
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn cqo_sign_fills_rdx() {
    diff(
        |a| {
            a.mov(rax, 0xFFFF_FFFF_FFFF_FFFFu64).unwrap(); // negative
            a.cqo().unwrap(); // rdx = all ones
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn setcc_from_compare() {
    diff(
        |a| {
            a.mov(eax, 3i32).unwrap();
            a.cmp(eax, 5i32).unwrap(); // 3 < 5 -> below/less
            a.setb(bl).unwrap(); // bl = 1
            a.setl(cl).unwrap(); // cl = 1
            a.setg(dl).unwrap(); // dl = 0
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn cmovcc_taken_and_not_taken() {
    diff(
        |a| {
            a.mov(eax, 1i32).unwrap();
            a.mov(ecx, 0x1111i32).unwrap();
            a.mov(edx, 0x2222i32).unwrap();
            a.cmp(eax, 0i32).unwrap(); // 1 > 0
            a.cmovg(ecx, edx).unwrap(); // taken -> ecx=0x2222
            a.cmovl(ecx, eax).unwrap(); // not taken -> ecx unchanged (but zero-extended)
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn shift_by_one_matches_unicorn() {
    // count == 1: OF is defined, so don't mask it (only AF is undefined).
    diff(
        |a| {
            a.mov(eax, 0xC000_0001u32 as i32).unwrap();
            a.shl(eax, 1i32).unwrap();
            a.mov(ebx, 0x0000_0003i32).unwrap();
            a.shr(ebx, 1i32).unwrap();
            a.mov(ecx, 0x8000_0004u32 as i32).unwrap();
            a.sar(ecx, 1i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Af],
    );
}

#[test]
fn shift_by_many_matches_unicorn() {
    // count > 1: OF is architecturally undefined -> mask OF and AF.
    diff(
        |a| {
            a.mov(rax, 0x1234_5678_9ABC_DEF0u64).unwrap();
            a.shl(rax, 5i32).unwrap();
            a.mov(rbx, 0xFEDC_BA98_7654_3210u64).unwrap();
            a.shr(rbx, 7i32).unwrap();
            a.sar(rbx, 3i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Af, FlagName::Of],
    );
}

#[test]
fn rotate_by_one_matches_unicorn() {
    // count == 1: CF and OF both defined. Rotates leave SF/ZF/PF/AF untouched.
    diff(
        |a| {
            a.mov(eax, 0x8000_0001u32 as i32).unwrap();
            a.rol(eax, 1i32).unwrap();
            a.mov(ebx, 0x0000_0003i32).unwrap();
            a.ror(ebx, 1i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn rotate_by_many_matches_unicorn() {
    // count > 1: OF undefined -> masked; CF still defined.
    diff(
        |a| {
            a.mov(rax, 0x1234_5678_9ABC_DEF0u64).unwrap();
            a.rol(rax, 20i32).unwrap();
            a.mov(ebx, 0xDEAD_BEEFu32 as i32).unwrap();
            a.ror(ebx, 7i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Of],
    );
}

#[test]
fn mul_imul_match_unicorn() {
    // mul/imul define only CF/OF; SF/ZF/PF/AF are undefined -> masked.
    diff(
        |a| {
            a.mov(eax, 0x0012_3456i32).unwrap();
            a.mov(ebx, 0x0000_789Ai32).unwrap();
            a.mul(ebx).unwrap();
            a.mov(eax, 50_000i32).unwrap();
            a.mov(ecx, 50_000i32).unwrap();
            a.imul_2(eax, ecx).unwrap(); // overflow -> CF/OF set
            a.mov(esi, 7i32).unwrap();
            a.imul_3(edx, esi, 3i32).unwrap(); // no overflow -> CF/OF clear
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Af, FlagName::Sf, FlagName::Zf, FlagName::Pf],
    );
}

#[test]
fn div_idiv_match_unicorn() {
    // div/idiv leave all flags undefined -> mask them; check RAX/RDX.
    diff(
        |a| {
            a.mov(edx, 0i32).unwrap();
            a.mov(eax, 1_000_003i32).unwrap();
            a.mov(ecx, 7i32).unwrap();
            a.div(ecx).unwrap();
            a.mov(eax, -1003i32).unwrap();
            a.mov(edx, -1i32).unwrap();
            a.mov(esi, 7i32).unwrap();
            a.idiv(esi).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Cf, FlagName::Pf, FlagName::Af, FlagName::Zf, FlagName::Sf, FlagName::Of],
    );
}

#[test]
fn high_byte_bswap_xchg_match_unicorn() {
    diff(
        |a| {
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.mov(dh, al).unwrap(); // write AH-family: dh = al
            a.movzx(ebx, ah).unwrap(); // read AH
            a.bswap(ecx).unwrap(); // (ecx=0, trivial) — exercise bswap
            a.mov(esi, 0x0A0B_0C0Di32).unwrap();
            a.bswap(esi).unwrap(); // -> 0x0D0C0B0A
            a.mov(rdi, 0xDEAD_BEEF_CAFE_B0BAu64).unwrap();
            a.bswap(rdi).unwrap();
            a.mov(r8, 0x1111u64).unwrap();
            a.mov(r9, 0x2222u64).unwrap();
            a.xchg(r8, r9).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn sse_matches_unicorn() {
    diff(
        |a| {
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.movq(xmm0, rax).unwrap();
            a.mov(rbx, 0xAABB_CCDD_EEFF_0011u64).unwrap();
            a.movq(xmm1, rbx).unwrap();
            a.pxor(xmm2, xmm2).unwrap();
            a.por(xmm2, xmm0).unwrap();
            a.pand(xmm2, xmm1).unwrap();
            a.pandn(xmm3, xmm1).unwrap(); // xmm3=0 -> andn gives xmm1
            a.movdqu(xmmword_ptr(SCRATCH), xmm2).unwrap();
            a.movdqu(xmm4, xmmword_ptr(SCRATCH)).unwrap();
            a.movdqa(xmm5, xmm4).unwrap();
            a.movd(ecx, xmm0).unwrap();
            a.movq(rdx, xmm1).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn packed_arith_shift_match_unicorn() {
    diff(
        |a| {
            a.mov(rax, 0x0000_0002_0000_0001u64).unwrap();
            a.movq(xmm0, rax).unwrap();
            a.mov(rax, 0x0000_0004_0000_0003u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.paddd(xmm0, xmm1).unwrap();
            a.psubd(xmm1, xmm0).unwrap();
            a.pcmpeqd(xmm2, xmm2).unwrap();
            a.mov(rax, 0xFF00_FF00_FF00_FF00u64).unwrap();
            a.movq(xmm3, rax).unwrap();
            a.pslld(xmm3, 4).unwrap();
            a.psrld(xmm3, 8).unwrap();
            a.psrlw(xmm3, 2).unwrap();
            a.paddb(xmm0, xmm3).unwrap();
            a.psrldq(xmm3, 5).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn float_scalar_matches_unicorn() {
    diff(float_scalar_body, |_| {}, &[]);
}

#[test]
fn float_packed_matches_unicorn() {
    diff(float_packed_body, |_| {}, &[]);
}

/// Scalar SSE2 double: cvtsi2sd/movsd/add/sub/mul/div, a memory source, both
/// convert-to-int roundings, precision converts, and a compare setting flags. All
/// values are exact IEEE doubles so the result is bit-stable against the CPU.
fn float_scalar_body(a: &mut CodeAssembler) {
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
fn float_packed_body(a: &mut CodeAssembler) {
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
    a.hlt().unwrap();
}

#[test]
fn shuffles_match_unicorn() {
    diff(
        |a| {
            a.mov(rax, 0x0706_0504_0302_0100u64).unwrap();
            a.movq(xmm0, rax).unwrap();
            a.mov(rax, 0x0F0E_0D0C_0B0A_0908u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.pshufd(xmm2, xmm0, 0x1B).unwrap();
            a.movdqa(xmm3, xmm0).unwrap();
            a.punpcklbw(xmm3, xmm1).unwrap();
            a.movdqa(xmm4, xmm0).unwrap();
            a.punpcklwd(xmm4, xmm1).unwrap();
            a.movdqa(xmm5, xmm0).unwrap();
            a.punpckldq(xmm5, xmm1).unwrap();
            a.movdqa(xmm6, xmm0).unwrap();
            a.punpcklqdq(xmm6, xmm1).unwrap();
            a.mov(rax, 0x00FF_0102_FF80_0040u64).unwrap(); // mix incl. negative-as-i16
            a.movq(xmm7, rax).unwrap();
            a.movdqa(xmm8, xmm7).unwrap();
            a.packuswb(xmm8, xmm7).unwrap();
            a.mov(ecx, 0xABCDi32).unwrap();
            a.pinsrw(xmm0, ecx, 3).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn string_ops_match_unicorn() {
    diff(
        |a| {
            a.cld().unwrap();
            // memset: fill 8 bytes at SCRATCH with 0x5A.
            a.mov(edi, SCRATCH as i32).unwrap();
            a.mov(ecx, 8i32).unwrap();
            a.mov(eax, 0x5Ai32).unwrap();
            a.rep().stosb().unwrap();
            // memcpy: copy those 8 bytes to SCRATCH+32.
            a.mov(esi, SCRATCH as i32).unwrap();
            a.mov(edi, (SCRATCH + 32) as i32).unwrap();
            a.mov(ecx, 8i32).unwrap();
            a.rep().movsb().unwrap();
            // repne scasb over the filled region.
            a.mov(edi, SCRATCH as i32).unwrap();
            a.mov(ecx, 8i32).unwrap();
            a.mov(al, 0x5Ai32).unwrap();
            a.repne().scasb().unwrap();
            // repe cmpsb comparing the two equal regions.
            a.mov(esi, SCRATCH as i32).unwrap();
            a.mov(edi, (SCRATCH + 32) as i32).unwrap();
            a.mov(ecx, 8i32).unwrap();
            a.repe().cmpsb().unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn call_and_ret() {
    diff(
        |a| {
            let mut func = a.create_label();
            a.call(func).unwrap();
            a.hlt().unwrap();
            a.set_label(&mut func).unwrap();
            a.mov(eax, 99i32).unwrap();
            a.ret().unwrap();
        },
        |cpu| cpu.gpr[4] = Vector::scratch(),
        &[],
    );
}
