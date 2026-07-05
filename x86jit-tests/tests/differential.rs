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
    Vector::asm(build)
        .init(init)
        .dont_care(dont_care)
        .assert_matches_unicorn();
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

/// `cbw`/`cwde`/`cdqe`: sign-extend the accumulator in place. Each writes a
/// different width (AL→AX merges into RAX; AX→EAX zeroes the upper 32; EAX→RAX).
#[test]
fn cbw_cwde_cdqe_match_unicorn() {
    diff(
        |a| {
            a.mov(rax, 0xFFFF_FFFF_FFFF_FF80u64).unwrap();
            a.cbw().unwrap(); // AL=0x80 → AX=0xFF80; bits above 16 preserved
            a.mov(rbx, rax).unwrap();
            a.mov(rax, 0x1111_1111_1111_8234u64).unwrap();
            a.cwde().unwrap(); // AX=0x8234 → EAX=0xFFFF8234; upper 32 zeroed
            a.mov(rcx, rax).unwrap();
            a.mov(rax, 0x0000_0000_9000_0000u64).unwrap();
            a.cdqe().unwrap(); // EAX=0x90000000 → RAX=0xFFFFFFFF90000000
            a.mov(rdx, rax).unwrap();
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
        &[
            FlagName::Cf,
            FlagName::Pf,
            FlagName::Af,
            FlagName::Zf,
            FlagName::Sf,
            FlagName::Of,
        ],
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
            a.movdqa(xmm4, xmm0).unwrap();
            a.pslldq(xmm4, 4).unwrap(); // byte-shift left (ld.so strcmp path)
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// x86-64-v2 (Jaguar-class) additions: SSSE3 `pshufb` (reg + mem), SSE4.1
/// `pextrb`/`pcmpeqq`, SSE4.2 `pcmpgtq`/`crc32`, and `popcnt`. Each must match the
/// CPU bit-for-bit — including `popcnt`'s ZF and the CRC-32C checksum.
#[test]
fn jaguar_v2_matches_unicorn() {
    diff(
        |a| {
            // 16 data bytes 00..0f and a shuffle mask (with a high-bit lane that
            // zeroes its output) at SCRATCH / SCRATCH+16.
            a.mov(rax, 0x0706_0504_0302_0100u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0x0f0e_0d0c_0b0a_0908u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm0, xmmword_ptr(SCRATCH)).unwrap();
            a.mov(rax, 0x8003_0201_0007_0f0eu64).unwrap();
            a.mov(qword_ptr(SCRATCH + 16), rax).unwrap();
            a.mov(rax, 0x0102_0304_0506_0708u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 24), rax).unwrap();
            a.movdqu(xmm1, xmmword_ptr(SCRATCH + 16)).unwrap();
            a.pshufb(xmm0, xmm1).unwrap(); // register index
            a.pshufb(xmm0, xmmword_ptr(SCRATCH + 16)).unwrap(); // memory index
            a.movdqu(xmmword_ptr(SCRATCH + 32), xmm0).unwrap();

            // pcmpgtq / pcmpeqq on 64-bit lanes.
            a.mov(rax, 5i64).unwrap();
            a.movq(xmm2, rax).unwrap();
            a.mov(rax, 3i64).unwrap();
            a.movq(xmm3, rax).unwrap();
            a.pcmpgtq(xmm2, xmm3).unwrap();
            a.pcmpeqq(xmm3, xmm3).unwrap();
            a.movdqu(xmmword_ptr(SCRATCH + 48), xmm2).unwrap();
            a.movdqu(xmmword_ptr(SCRATCH + 64), xmm3).unwrap();

            // pextrb into a gpr.
            a.pextrb(edx, xmm0, 3i32).unwrap();

            // crc32 accumulate a byte then a qword.
            a.mov(ecx, 0i32).unwrap();
            a.mov(rsi, 0x1122_3344_5566_7788u64).unwrap();
            a.crc32(ecx, sil).unwrap();
            a.crc32(rcx, rsi).unwrap();

            // popcnt last, so ZF (and the cleared flags) reach hlt.
            a.mov(rax, 0xF0F0_F0F0_1234_5678u64).unwrap();
            a.popcnt(rbx, rax).unwrap();
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

#[test]
fn atomics_match_unicorn() {
    diff(atomics_body, |_| {}, &[]);
}

#[test]
fn x87_matches_unicorn() {
    // Exactly-representable values only, so f64-backed x87 equals the real 80-bit
    // FPU. Results read back into GPRs; the x87 stack itself isn't compared.
    diff(x87_body, |_| {}, &[]);
}

/// x87 stack arithmetic, int/float load-store, fchs/fabs, and a compare.
fn x87_body(a: &mut CodeAssembler) {
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

#[test]
fn x87_fistp_honors_rounding_control_matches_unicorn() {
    // `fldcw` sets the RC field; `fistp` must round per that mode, not always
    // ties-to-even (#8). 1.5 rounds differently under each mode, so a mode-ignoring
    // fistp diverges from the real FPU.
    diff(x87_fistp_rounding_body, |_| {}, &[]);
}

/// Set the control word to `cw`, load the exactly-representable f64 `bits`, `fistp`
/// it to a dword, and read the result into `dst` (a 32-bit GPR encoding).
fn fistp_under_cw(a: &mut CodeAssembler, cw: u64, bits: u64, dst: AsmRegister32) {
    a.mov(rax, cw).unwrap();
    a.mov(qword_ptr(SCRATCH), rax).unwrap();
    a.fldcw(word_ptr(SCRATCH)).unwrap();
    a.mov(rax, bits).unwrap();
    a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
    a.fld(qword_ptr(SCRATCH + 8)).unwrap();
    a.fistp(dword_ptr(SCRATCH + 16)).unwrap();
    a.mov(dst, dword_ptr(SCRATCH + 16)).unwrap();
}

fn x87_fistp_rounding_body(a: &mut CodeAssembler) {
    const ONE_HALF: u64 = 0x3FF8_0000_0000_0000; // 1.5, exactly representable
    // Control words (base 0x037F) with each RC field (bits 10-11).
    fistp_under_cw(a, 0x0F7F, ONE_HALF, r8d); // truncate  -> 1
    fistp_under_cw(a, 0x037F, ONE_HALF, r9d); // nearest   -> 2 (ties to even)
    fistp_under_cw(a, 0x0B7F, ONE_HALF, r10d); // up (+inf) -> 2
    fistp_under_cw(a, 0x077F, ONE_HALF, r11d); // down(-inf)-> 1
    a.hlt().unwrap();
}

#[test]
fn x87_subnormal_fstp_tbyte_matches_unicorn() {
    // `fld` of a subnormal f64 normalizes it into the 80-bit register; `fstp tbyte`
    // stores the exact f80. The f64->f80 encoder dropped the top fraction bit of
    // multi-bit subnormals (#8), so the stored mantissa was off by up to 2x.
    diff(x87_subnormal_body, |_| {}, &[]);
}

fn x87_subnormal_body(a: &mut CodeAssembler) {
    // Raw bits 3 = the subnormal 3 * 2^-1074 (two significant fraction bits).
    a.mov(rax, 3u64).unwrap();
    a.mov(qword_ptr(SCRATCH), rax).unwrap();
    a.fld(qword_ptr(SCRATCH)).unwrap();
    a.fstp(tbyte_ptr(SCRATCH + 16)).unwrap();
    a.mov(r8, qword_ptr(SCRATCH + 16)).unwrap(); // 64-bit mantissa (integer bit + frac)
    a.movzx(r9, word_ptr(SCRATCH + 24)).unwrap(); // sign + 15-bit exponent
    a.hlt().unwrap();
}

#[test]
fn x87_register_and_width_forms_match_unicorn() {
    // Register-form arithmetic (ST0-dest, no pop), register fst copy, m32 memory
    // operands, and a 16-bit fistp — the forms lift_x87 previously misrouted to
    // *MemF64 with a dummy address 0 (fst/fsub/fdiv reg-form), read 8 bytes as f64
    // for an m32 operand, or wrote 4 bytes for `fistp word`. All exactly-
    // representable values so f64-backed x87 equals the real 80-bit FPU.
    diff(x87_reg_width_body, |_| {}, &[]);
}

/// Load an exactly-representable f64 (given as raw bits) onto the x87 stack via a
/// staging slot at `SCRATCH`.
fn push_f64(a: &mut CodeAssembler, bits: u64) {
    a.mov(rax, bits).unwrap();
    a.mov(qword_ptr(SCRATCH), rax).unwrap();
    a.fld(qword_ptr(SCRATCH)).unwrap();
}

fn x87_reg_width_body(a: &mut CodeAssembler) {
    const TEN: u64 = 0x4024_0000_0000_0000;
    const THREE: u64 = 0x4008_0000_0000_0000;
    const TWELVE: u64 = 0x4028_0000_0000_0000;
    const FORTYEIGHT: u64 = 0x4048_0000_0000_0000;
    const NINE: u64 = 0x4022_0000_0000_0000;
    const FIVE: u64 = 0x4014_0000_0000_0000;

    // fsub st0, st1 : ST0 = 3 - 10 = -7  (reg form, previously wrote addr 0)
    push_f64(a, TEN);
    push_f64(a, THREE);
    a.fsub_2(st0, st1).unwrap();
    a.fstp(qword_ptr(SCRATCH + 8)).unwrap(); // store -7, pop
    a.fstp(st0).unwrap(); // discard the 10 (exercises FstpSti)
    a.mov(r8, qword_ptr(SCRATCH + 8)).unwrap();

    // fsubr st0, st1 : ST0 = 10 - 3 = 7
    push_f64(a, TEN);
    push_f64(a, THREE);
    a.fsubr_2(st0, st1).unwrap();
    a.fstp(qword_ptr(SCRATCH + 16)).unwrap();
    a.fstp(st0).unwrap();
    a.mov(r9, qword_ptr(SCRATCH + 16)).unwrap();

    // fdiv st0, st1 : ST0 = 3 / 12 = 0.25
    push_f64(a, TWELVE);
    push_f64(a, THREE);
    a.fdiv_2(st0, st1).unwrap();
    a.fstp(qword_ptr(SCRATCH + 24)).unwrap();
    a.fstp(st0).unwrap();
    a.mov(r10, qword_ptr(SCRATCH + 24)).unwrap();

    // fdivr st0, st1 : ST0 = ST1 / ST0 = 12 / 48 = 0.25
    push_f64(a, FORTYEIGHT);
    push_f64(a, TWELVE);
    a.fdivr_2(st0, st1).unwrap();
    a.fstp(qword_ptr(SCRATCH + 32)).unwrap();
    a.fstp(st0).unwrap();
    a.mov(r11, qword_ptr(SCRATCH + 32)).unwrap();

    // fst st1 : copy ST0 into ST1 (no pop). If broken it wrote to addr 0 and ST1
    // stayed 9; the store below then reads 5 iff the copy happened.
    push_f64(a, NINE);
    push_f64(a, FIVE);
    a.fst(st1).unwrap(); // ST1 = ST0 = 5
    a.fstp(st0).unwrap(); // pop the 5, ST0 now = ST1 (5 if copy worked, else 9)
    a.fstp(qword_ptr(SCRATCH + 40)).unwrap();
    a.mov(r12, qword_ptr(SCRATCH + 40)).unwrap();

    // fdiv dword[m] : m32 operand. 10 / 4.0f32 = 2.5. Previously read 8 bytes as f64.
    a.mov(dword_ptr(SCRATCH + 48), 0x4080_0000u32 as i32).unwrap(); // 4.0f32
    push_f64(a, TEN);
    a.fdiv(dword_ptr(SCRATCH + 48)).unwrap();
    a.fstp(qword_ptr(SCRATCH + 56)).unwrap();
    a.mov(r13, qword_ptr(SCRATCH + 56)).unwrap();

    // fistp word[m] : 16-bit store must touch only 2 bytes. Pre-seed the dword with
    // a sentinel; a correct 2-byte store leaves the upper half intact.
    a.mov(dword_ptr(SCRATCH + 64), 0xAAAA_BBBBu32 as i32).unwrap();
    a.mov(dword_ptr(SCRATCH + 72), 5i32).unwrap();
    a.fild(dword_ptr(SCRATCH + 72)).unwrap();
    a.fistp(word_ptr(SCRATCH + 64)).unwrap(); // writes low 2 bytes = 5
    a.mov(r14d, dword_ptr(SCRATCH + 64)).unwrap(); // = 0xAAAA0005 iff only 2 bytes written

    // fstp st(1): ST(1) = ST(0), then pop -> new ST0 = old ST0. The register-copy
    // pop lua uses heavily; the old memory-form bug wrote 8 bytes to addr 0 instead.
    push_f64(a, NINE); // ST1 slot
    push_f64(a, FIVE); // ST0
    a.fstp(st1).unwrap();
    a.fstp(qword_ptr(SCRATCH + 80)).unwrap();
    a.mov(r15, qword_ptr(SCRATCH + 80)).unwrap();

    // ST(i)-destination register arithmetic (op0 = ST(i)): `fmul st(1), st(0)` and
    // `fsub st(1), st(0)` write ST(1), not ST(0) — the *ToSti kinds. (lua uses
    // `fmul %st,%st(1)`; the previous lift wrote the result to ST(0).)
    push_f64(a, FIVE); // ST1
    push_f64(a, THREE); // ST0
    a.fmul_2(st1, st0).unwrap(); // ST1 = 5 * 3 = 15
    a.fstp(st0).unwrap(); // drop ST0=3, ST0 now = 15
    a.fstp(qword_ptr(SCRATCH + 88)).unwrap();
    a.mov(rbx, qword_ptr(SCRATCH + 88)).unwrap();

    push_f64(a, TEN); // ST1
    push_f64(a, THREE); // ST0
    a.fsub_2(st1, st0).unwrap(); // ST1 = 10 - 3 = 7
    a.fstp(st0).unwrap();
    a.fstp(qword_ptr(SCRATCH + 96)).unwrap();
    a.mov(rcx, qword_ptr(SCRATCH + 96)).unwrap();
    a.hlt().unwrap();
}

#[test]
fn bitscan_and_cdq_match_unicorn() {
    // bsf/bsr define ZF; the other flags are undefined.
    diff(
        bitscan_cdq_body,
        |_| {},
        &[
            FlagName::Of,
            FlagName::Sf,
            FlagName::Cf,
            FlagName::Af,
            FlagName::Pf,
        ],
    );
}

#[test]
fn sse_half_moves_match_unicorn() {
    diff(sse_half_body, |_| {}, &[]);
}

#[test]
fn sse_string_ops_match_unicorn() {
    diff(sse_string_body, |_| {}, &[]);
}

#[test]
fn sse_shuffle_cmp_match_unicorn() {
    diff(sse_shuffle_cmp_body, |_| {}, &[]);
}

/// shufps/shufpd, cmpltsd, psraw/psrad, punpckh*, and pshufd with a memory source.
fn sse_shuffle_cmp_body(a: &mut CodeAssembler) {
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
fn sse_string_body(a: &mut CodeAssembler) {
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
fn bitscan_cdq_body(a: &mut CodeAssembler) {
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
fn sse_half_body(a: &mut CodeAssembler) {
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

#[test]
fn bit_test_matches_unicorn() {
    // bt* define CF; OF/SF/ZF/AF/PF are architecturally undefined.
    diff(
        bit_test_body,
        |_| {},
        &[
            FlagName::Of,
            FlagName::Sf,
            FlagName::Zf,
            FlagName::Af,
            FlagName::Pf,
        ],
    );
}

/// bt/bts/btr/btc with register and immediate indices, register and memory
/// operands; CF captured per-op via `setb`, writebacks read into registers.
fn bit_test_body(a: &mut CodeAssembler) {
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
fn atomics_body(a: &mut CodeAssembler) {
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
