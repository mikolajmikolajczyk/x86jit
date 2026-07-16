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

/// Validate a VEX.128 snippet against the equivalent legacy-SSE snippet on the
/// interpreter (task-168.1). Unicorn's QEMU build mis-decodes VEX 3-operand forms
/// (it drops `vvvv`), so it can't be the AVX oracle; instead we assert the new VEX
/// lowering produces the same state as the already-trusted SSE lowering (which the
/// whole differential corpus validates against Unicorn). Both snippets get the same
/// `init`, so any lowering bug in the VEX arm shows as a divergence.
fn vex_eq_sse(
    vex: impl FnOnce(&mut CodeAssembler),
    sse: impl FnOnce(&mut CodeAssembler),
    init: impl Fn(&mut CpuSnapshot),
) {
    let v = Vector::asm(vex).init(&init).interpret();
    let s = Vector::asm(sse).init(&init).interpret();
    // Compare the observable data state (xmm + gpr); RIP legitimately differs because
    // the VEX and SSE snippets are different lengths, and neither op set touches flags.
    assert_eq!(v.cpu.xmm, s.cpu.xmm, "xmm state: VEX.128 vs SSE equivalent");
    assert_eq!(v.cpu.gpr, s.cpu.gpr, "gpr state: VEX.128 vs SSE equivalent");
}

/// task-258: validate a VEX.256 snippet against an SSE-per-half reference on the interpreter.
/// Unicorn mis-decodes VEX and can't oracle 256-bit ops; the native-AVX oracle in `native.rs`
/// is the hardware ground truth. This helper is the *structural* check that runs everywhere
/// (no AVX host needed): the `sse` closure computes the same result by splitting each ymm into
/// its two 128-bit halves with the already-trusted `vextractf128`/`vinsertf128` and applying
/// the legacy-SSE op to each half. Compares the full 256-bit state (`xmm` + `ymm_hi`).
fn vex256_eq_sse(
    vex: impl FnOnce(&mut CodeAssembler),
    sse: impl FnOnce(&mut CodeAssembler),
    init: impl Fn(&mut CpuSnapshot),
) {
    let v = Vector::asm(vex).init(&init).interpret();
    let s = Vector::asm(sse).init(&init).interpret();
    // The SSE-per-half reference dirties scratch ymm10..=13 that the VEX arm doesn't touch,
    // so compare only the result registers ymm2..=9 (both 128-bit halves). Inputs are ymm0/1;
    // scratch is ymm10..=13.
    for r in 2..=9usize {
        assert_eq!(
            (v.cpu.xmm[r], v.cpu.ymm_hi[r]),
            (s.cpu.xmm[r], s.cpu.ymm_hi[r]),
            "ymm{r} (256-bit) state: VEX.256 vs SSE-per-half"
        );
    }
    assert_eq!(v.cpu.gpr, s.cpu.gpr, "gpr state: VEX.256 vs SSE-per-half");
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

/// task-164: non-temporal stores `movntdq`/`movntps`/`movntpd` (16-byte vector) and
/// `movnti` (GPR) lower to plain stores in our coherent model. Store to scratch, load
/// back, and assert the round-tripped bytes match Unicorn — proves the store landed.
#[test]
fn movnt_stores_match_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            // Vector non-temporal store, then read back via movdqu.
            a.movntdq(xmmword_ptr(rax), xmm1).unwrap();
            a.movdqu(xmm2, xmmword_ptr(rax + 16)).unwrap(); // untouched region -> 0
            a.movdqu(xmm3, xmmword_ptr(rax)).unwrap(); // the stored value
                                                       // GPR non-temporal store, then read back.
            a.movnti(qword_ptr(rax + 32), rbx).unwrap();
            a.mov(rcx, qword_ptr(rax + 32)).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
            c.gpr[3] = 0xDEAD_BEEF_CAFE_F00D; // rbx
        },
        &[],
    );
}

/// task-189: 8-bit one-operand `mul r/m8` / `imul r/m8` validated against Unicorn.
/// AL*src8 → AX (AH:AL); only CF/OF are architecturally defined, so SF/ZF/AF/PF are
/// masked. Exercises overflow (AH != 0) and the signed-negative product.
#[test]
fn mul8_imul8_match_unicorn() {
    diff(
        |a| {
            a.mov(al, 0xFFi32).unwrap();
            a.mov(bl, 0x12i32).unwrap();
            a.mul(bl).unwrap(); // 0xFF*0x12 = 0x11EE -> AX, CF/OF set
            a.mov(al, (-3i32) & 0xFF).unwrap(); // 0xFD
            a.mov(dl, 4i32).unwrap();
            a.imul(dl).unwrap(); // -3*4 = -12 -> AX = 0xFFF4, CF/OF set
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Sf, FlagName::Zf, FlagName::Af, FlagName::Pf],
    );
}

/// task-248: 8-bit one-operand `div r/m8` / `idiv r/m8` (F6 /6,/7) validated against
/// Unicorn. Dividend is the 16-bit AX (not RDX:RAX); quotient → AL, remainder → AH.
/// div/idiv leave all flags undefined, so all six are masked. Covers unsigned, signed
/// with a negative dividend, `dil` reached via REX (the exact retail wall shape), and a
/// non-zero remainder — plus a dirtied RAX upper so "only AX read / AL:AH written" shows.
#[test]
fn div8_idiv8_match_unicorn() {
    diff(
        |a| {
            // Dirty RAX[63:16] so the AX-only dividend read is observable.
            a.mov(rax, 0x7777_7777_7777_0000u64).unwrap();
            a.mov(ax, 1003i32).unwrap(); // AX = 0x3EB
            a.mov(bl, 10i32).unwrap();
            a.div(bl).unwrap(); // 1003 / 10 = 100 rem 3 -> AL=100, AH=3
                                // Signed: AX = -100 (0xFF9C), divisor -7 -> idiv via dil (REX). Quotient 14
                                // fits signed i8; remainder -2 (sign of dividend).
            a.mov(ax, (-100i32) & 0xFFFF).unwrap();
            a.mov(dil, (-7i32) & 0xFF).unwrap();
            a.idiv(dil).unwrap(); // -100 / -7 = 14 rem -2 -> AL=14, AH=0xFE
                                  // Mixed sign -> negative quotient (a sign bug that keeps
                                  // neg/neg positive would slip past the case above).
            a.mov(ax, (-100i32) & 0xFFFF).unwrap();
            a.mov(cl, 7i32).unwrap();
            a.idiv(cl).unwrap(); // -100 / 7 = -14 rem -2 -> AL=0xF2, AH=0xFE
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
fn rotate_through_carry_by_one_matches_unicorn() {
    // rcl/rcr, count == 1: CF-in is CONSUMED (rotate through carry), CF/OF both defined.
    // `48 D1 DB` (rcr rbx,1) is the exact opcode Go's div-by-constant carry fold emits
    // that trapped the netpoller (task-132). Test both CF-in states via stc/clc.
    diff(
        |a| {
            a.mov(rbx, 0x8000_0000_0000_0001u64).unwrap();
            a.rcr(rbx, 1u32).unwrap(); // CF-in = 1 (from init) rotates into bit 63
            a.mov(ecx, 0x0000_0001i32).unwrap();
            a.rcl(ecx, 1u32).unwrap(); // CF-in = rcr's CF-out
            a.hlt().unwrap();
        },
        |a| a.flags.cf = true,
        &[],
    );
}

#[test]
fn rotate_through_carry_widths_and_counts_match_unicorn() {
    // 8/16/32/64-bit and count > 1 (OF undefined -> masked; CF defined).
    diff(
        |a| {
            a.mov(al, 0x81u32 as i32).unwrap();
            a.rcr(al, 1u32).unwrap();
            a.rcl(bx, 3u32).unwrap();
            a.rcr(edx, 5u32).unwrap();
            a.rcl(rsi, 30u32).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.flags.cf = true; // CF-in for the first rotate
            a.gpr[2] = 0x1234_5678; // rdx
            a.gpr[6] = 0xFEDC_BA98_7654_3210; // rsi
        },
        &[FlagName::Of],
    );
}

#[test]
fn div_by_constant_carry_fold_matches_unicorn() {
    // The unsigned divide-by-constant shape Go emits: magic multiply, add the high half
    // (which can carry out of 64 bits), then `rcr r,1` folds that carry back into bit 63
    // before the final shift. This is the exact instruction pattern that walled the Go
    // netpoller (task-132) — here validated end to end against the Unicorn oracle.
    diff(
        |a| {
            a.mov(rbx, 0xFFFF_FFFF_FFFF_FFF0u64).unwrap();
            a.mov(rax, 0x2492_4924_9249_2493u64).unwrap(); // ÷7 magic
            a.mul(rbx).unwrap(); // rdx:rax = rbx * magic
            a.add(rbx, rdx).unwrap(); // may carry out of 64 bits -> CF
            a.rcr(rbx, 1u32).unwrap(); // fold CF into bit 63
            a.shr(rbx, 2u32).unwrap(); // final shift
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Of, FlagName::Af], // final op is shr by 2: OF/AF undefined
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

/// `cmpss` (F3 0F C2 /r ib): scalar-single compare with the 8 legacy imm8
/// predicates EQ/LT/LE/UNORD/NEQ/NLT/NLE/ORD (0..7). Writes an all-ones/0 32-bit
/// mask into the dest's low dword; the upper 96 bits of the dest are preserved.
/// Runs every predicate with BOTH a register and a memory second operand into
/// distinct xmm regs so the whole result register file is asserted against the
/// host CPU. `xmm1` = 2.0f, memory (SCRATCH) also holds 2.0f; `xmm0` is set per
/// case. Includes an equal case (2==2) and a less-than case (1<2) so the ordered
/// vs unordered/negated predicates all take a non-trivial branch. The upper dword
/// of each dest is pre-dirtied via the `xmm2` seed copy to prove it survives.
fn cmpss_pred_body(a: &mut CodeAssembler, lhs_bits: u32) {
    // 2.0f in xmm1 and in scratch memory (the register vs memory comparand).
    a.mov(eax, 0x4000_0000u32 as i32).unwrap(); // 2.0f
    a.movd(xmm1, eax).unwrap();
    a.mov(dword_ptr(SCRATCH), eax).unwrap();
    // lhs in the low dword; a distinct pattern in the upper 96 bits to prove
    // the scalar compare leaves them untouched.
    a.mov(rax, 0xDEAD_BEEF_0000_0000u64 | lhs_bits as u64)
        .unwrap();
    a.movq(xmm15, rax).unwrap();
    a.mov(rax, 0xCAFE_F00D_1122_3344u64).unwrap();
    a.pinsrq(xmm15, rax, 1).unwrap(); // xmm15 = [upper96 | lhs]

    // For each predicate, reload the seed into a fresh dest and compare.
    // Register-operand forms → xmm2..=xmm9, memory-operand forms → xmm10..=xmm14 + xmm0.
    let reg_dsts = [xmm2, xmm3, xmm4, xmm5, xmm6, xmm7, xmm8, xmm9];
    let mem_dsts = [xmm10, xmm11, xmm12, xmm13, xmm14, xmm0];
    for (pred, dst) in reg_dsts.iter().enumerate() {
        a.movdqa(*dst, xmm15).unwrap();
        a.cmpss(*dst, xmm1, pred as u32).unwrap();
    }
    // Memory-operand form for the first six predicates (enough to cover an
    // ordered, an unordered, and a negated predicate against a mem source).
    for (pred, dst) in mem_dsts.iter().enumerate() {
        a.movdqa(*dst, xmm15).unwrap();
        a.cmpss(*dst, dword_ptr(SCRATCH), pred as u32).unwrap();
    }
    a.hlt().unwrap();
}

#[test]
fn cmpss_equal_matches_unicorn() {
    // lhs = 2.0f == the 2.0f comparand → EQ/LE/NLT/NLE/ORD true, LT/UNORD/NEQ false.
    diff(|a| cmpss_pred_body(a, 0x4000_0000), |_| {}, &[]);
}

#[test]
fn cmpss_less_than_matches_unicorn() {
    // lhs = 1.0f < 2.0f → LT/LE/NEQ/ORD true, EQ/UNORD/NLT/NLE false.
    diff(|a| cmpss_pred_body(a, 0x3F80_0000), |_| {}, &[]);
}

/// `cmpss` with a NaN operand: the unordered case is where the predicate
/// families diverge — ordered comparisons (EQ/LT/LE/ORD) are false, the
/// unordered/negated ones (UNORD/NEQ/NLT/NLE) are true. Tests both operand
/// orders (NaN as lhs and NaN as the comparand) since a quiet-NaN in either
/// slot makes `partial_cmp` unordered. Uses a QNaN (0x7FC0_0000).
fn cmpss_nan_body(a: &mut CodeAssembler, nan_is_lhs: bool) {
    let qnan = 0x7FC0_0000u32; // QNaN
    let two = 0x4000_0000u32; // 2.0f
    let (lhs, rhs) = if nan_is_lhs { (qnan, two) } else { (two, qnan) };
    a.mov(eax, rhs as i32).unwrap();
    a.movd(xmm1, eax).unwrap();
    a.mov(dword_ptr(SCRATCH), eax).unwrap();
    a.mov(eax, lhs as i32).unwrap();
    a.movd(xmm15, eax).unwrap();
    let reg_dsts = [xmm2, xmm3, xmm4, xmm5, xmm6, xmm7, xmm8, xmm9];
    let mem_dsts = [xmm10, xmm11, xmm12, xmm13, xmm14, xmm0];
    for (pred, dst) in reg_dsts.iter().enumerate() {
        a.movdqa(*dst, xmm15).unwrap();
        a.cmpss(*dst, xmm1, pred as u32).unwrap();
    }
    for (pred, dst) in mem_dsts.iter().enumerate() {
        a.movdqa(*dst, xmm15).unwrap();
        a.cmpss(*dst, dword_ptr(SCRATCH), pred as u32).unwrap();
    }
    a.hlt().unwrap();
}

#[test]
fn cmpss_nan_lhs_matches_unicorn() {
    diff(|a| cmpss_nan_body(a, true), |_| {}, &[]);
}

#[test]
fn cmpss_nan_rhs_matches_unicorn() {
    diff(|a| cmpss_nan_body(a, false), |_| {}, &[]);
}

/// VEX `vcmp{ss,sd,ps,pd}` (VEX.128): the 3-operand `dst = cmp(src1, src2)` form.
/// Validate against the legacy 2-operand SSE lowering (already Unicorn-trusted) — a
/// VEX.128 op zeroes bits 255:128, and the SSE form leaves them, so we assert only the
/// low-128 (xmm) data state, which must be identical. Both a register and a memory
/// second source, and every one of the 8 legacy predicates, across scalar+packed. The
/// SSE mirror copies src1 into the dest first (the 2-operand form is `dst OP= src2`),
/// so the two snippets compute the same value into distinct dests.
#[test]
fn vcmp_vex128_eq_sse() {
    // Distinct src1/src2 patterns so a dropped `vvvv` (compare-in-place bug) would show.
    let seed = |c: &mut CpuSnapshot| {
        // src1 = xmm0, src2 = xmm1; a mix of equal/less/greater/NaN lanes.
        c.xmm[0] = 0x7FC0_0000_4000_0000_3F80_0000_4000_0000; // f32: 2,1,2,NaN
        c.xmm[1] = 0x4000_0000_4000_0000_4000_0000_4000_0000; // f32: 2,2,2,2
    };
    for pred in 0u32..8 {
        vex_eq_sse(
            |a| {
                a.mov(rax, SCRATCH).unwrap();
                a.vmovdqu(xmmword_ptr(rax), xmm1).unwrap(); // src2 also in memory
                a.vcmpps(xmm2, xmm0, xmm1, pred).unwrap();
                a.vcmppd(xmm3, xmm0, xmm1, pred).unwrap();
                a.vcmpss(xmm4, xmm0, xmm1, pred).unwrap();
                a.vcmpsd(xmm5, xmm0, xmm1, pred).unwrap();
                a.vcmpps(xmm6, xmm0, xmmword_ptr(rax), pred).unwrap(); // mem src2
                a.vcmpsd(xmm7, xmm0, qword_ptr(rax), pred).unwrap(); // scalar mem src2
            },
            |a| {
                a.mov(rax, SCRATCH).unwrap();
                a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
                a.movdqa(xmm2, xmm0).unwrap();
                a.cmpps(xmm2, xmm1, pred).unwrap();
                a.movdqa(xmm3, xmm0).unwrap();
                a.cmppd(xmm3, xmm1, pred).unwrap();
                a.movdqa(xmm4, xmm0).unwrap();
                a.cmpss(xmm4, xmm1, pred).unwrap();
                a.movdqa(xmm5, xmm0).unwrap();
                a.cmpsd_3(xmm5, xmm1, pred).unwrap();
                a.movdqa(xmm6, xmm0).unwrap();
                a.cmpps(xmm6, xmmword_ptr(rax), pred).unwrap();
                a.movdqa(xmm7, xmm0).unwrap();
                a.cmpsd_3(xmm7, qword_ptr(rax), pred).unwrap();
            },
            seed,
        );
    }
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
fn shld_shrd_match_unicorn() {
    // Double-precision shifts (busybox `sort` uses SHLD). AF is undefined for these;
    // the final op is a count-1 SHLD so OF is defined.
    diff(shld_shrd_body, |_| {}, &[FlagName::Af]);
}

fn shld_shrd_body(a: &mut CodeAssembler) {
    // SHLD r64, r64, imm8
    a.mov(rax, 0x1234_5678_9abc_def0u64).unwrap();
    a.mov(rbx, 0xfedc_ba98_7654_3210u64).unwrap();
    a.shld(rax, rbx, 8i32).unwrap();
    a.mov(r12, rax).unwrap();
    // SHRD r64, r64, imm8
    a.mov(rax, 0x0000_0000_ffff_0000u64).unwrap();
    a.mov(rbx, 0x0000_0000_0000_00ffu64).unwrap();
    a.shrd(rax, rbx, 4i32).unwrap();
    a.mov(r13, rax).unwrap();
    // SHLD r32, r32, CL — count from CL, and the 32-bit upper-zeroing path.
    a.mov(eax, 0x8000_0001u32).unwrap();
    a.mov(ebx, 0x4000_0000u32).unwrap();
    a.mov(cl, 3i32).unwrap();
    a.shld(eax, ebx, cl).unwrap();
    a.mov(r14, rax).unwrap();
    // SHRD r32, r32, CL
    a.mov(eax, 0x0000_00ffu32).unwrap();
    a.mov(ebx, 0xff00_0000u32).unwrap();
    a.shrd(eax, ebx, cl).unwrap();
    a.mov(r15, rax).unwrap();
    // Final op: count-1 SHLD (OF defined) — flips the top bit, so OF/SF/CF all matter.
    a.mov(rax, 0xc000_0000_0000_0000u64).unwrap();
    a.mov(rbx, 0x8000_0000_0000_0000u64).unwrap();
    a.shld(rax, rbx, 1i32).unwrap();
    a.hlt().unwrap();
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

/// task-213: `fistp` of a value in [0.5, 1) previously panicked (`to_i64_rc` shift
/// overflow for exp == -1). Exercise every rounding mode across that range + a negative,
/// bit-exact vs the real FPU.
#[test]
fn x87_fistp_subunit_range_matches_unicorn() {
    diff(x87_fistp_subunit_body, |_| {}, &[]);
}

fn x87_fistp_subunit_body(a: &mut CodeAssembler) {
    const HALF: u64 = 0x3FE0_0000_0000_0000; // 0.5
    const THREE_Q: u64 = 0x3FE8_0000_0000_0000; // 0.75
    const NINE_T: u64 = 0x3FEC_CCCC_CCCC_CCCD; // ~0.9
    const NEG_3Q: u64 = 0xBFE8_0000_0000_0000; // -0.75
                                               // Control words: 0x037F nearest, 0x0F7F truncate, 0x0B7F up, 0x077F down.
    fistp_under_cw(a, 0x037F, HALF, r8d); // nearest → 0 (ties even)
    fistp_under_cw(a, 0x0B7F, HALF, r9d); // up → 1
    fistp_under_cw(a, 0x037F, THREE_Q, r10d); // nearest → 1
    fistp_under_cw(a, 0x0F7F, THREE_Q, r11d); // truncate → 0
    fistp_under_cw(a, 0x037F, NINE_T, r12d); // nearest → 1
    fistp_under_cw(a, 0x077F, NEG_3Q, r13d); // down(-inf) → -1
    fistp_under_cw(a, 0x0F7F, NEG_3Q, r14d); // truncate → 0
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
    a.mov(dword_ptr(SCRATCH + 48), 0x4080_0000u32 as i32)
        .unwrap(); // 4.0f32
    push_f64(a, TEN);
    a.fdiv(dword_ptr(SCRATCH + 48)).unwrap();
    a.fstp(qword_ptr(SCRATCH + 56)).unwrap();
    a.mov(r13, qword_ptr(SCRATCH + 56)).unwrap();

    // fistp word[m] : 16-bit store must touch only 2 bytes. Pre-seed the dword with
    // a sentinel; a correct 2-byte store leaves the upper half intact.
    a.mov(dword_ptr(SCRATCH + 64), 0xAAAA_BBBBu32 as i32)
        .unwrap();
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

// ---- task-188: deepened x87 differential (full stack + inexact + transcendentals) ----

/// AC#2: basic x87 arithmetic on operands whose true result is NOT representable in
/// 64 significand bits must match the real 80-bit FPU BIT-EXACTLY — no tolerance.
/// The old tests used only exactly-representable values and read results back into
/// GPRs (truncating to f64), so a wrong low mantissa bit was invisible; here the full
/// 80-bit ST(0) is left on the stack and compared against Unicorn's ST0 (task-188 §1).
#[test]
fn x87_inexact_arithmetic_matches_unicorn() {
    diff(x87_inexact_body, |_| {}, &[]);
}

/// Leaves four rounding-sensitive results on the x87 stack (ST0..ST3), each a
/// repeating-fraction quotient/product that needs all 64 significand bits: the
/// comparator asserts every ST byte matches Unicorn.
fn x87_inexact_body(a: &mut CodeAssembler) {
    const TEN: u64 = 0x4024_0000_0000_0000; // 10.0
    const THREE: u64 = 0x4008_0000_0000_0000; // 3.0
    const SEVEN: u64 = 0x401C_0000_0000_0000; // 7.0
    const ONE: u64 = 0x3FF0_0000_0000_0000; // 1.0
    const TWO: u64 = 0x4000_0000_0000_0000; // 2.0

    // 10 / 3 = 3.333… — non-terminating in binary, so the f80 result uses the full
    // 64-bit significand. A f64-backed register file would drop the low 11 bits.
    push_f64(a, THREE);
    push_f64(a, TEN);
    a.fdiv_2(st0, st1).unwrap(); // ST0 = 10 / 3
    a.fstp(st1).unwrap(); // drop the divisor, keep the quotient as ST0

    // 1 / 7 = 0.142857… — likewise inexact.
    push_f64(a, SEVEN);
    push_f64(a, ONE);
    a.fdiv_2(st0, st1).unwrap(); // ST0 = 1 / 7
    a.fstp(st1).unwrap();

    // (10 / 3) * 7 — an inexact product of an inexact operand: exercises fmul rounding.
    push_f64(a, THREE);
    push_f64(a, TEN);
    a.fdiv_2(st0, st1).unwrap();
    a.fstp(st1).unwrap();
    push_f64(a, SEVEN);
    a.fmul_2(st0, st1).unwrap(); // ST0 = (10/3) * 7
    a.fstp(st1).unwrap();

    // 2 / 3 — a third inexact quotient, kept as the deepest live register.
    push_f64(a, THREE);
    push_f64(a, TWO);
    a.fdiv_2(st0, st1).unwrap();
    a.fstp(st1).unwrap();

    a.hlt().unwrap();
}

/// f64 ULP distance between two finite values (bit-monotonic key).
fn transcendental_ulp_diff(a: f64, b: f64) -> u64 {
    fn key(x: f64) -> i64 {
        let b = x.to_bits() as i64;
        if b < 0 {
            i64::MIN - b
        } else {
            b
        }
    }
    key(a).abs_diff(key(b))
}

/// Run `build` on the interpreter and read ST(`i`) rounded to f64 (task-206).
fn interp_st_f64(build: impl FnOnce(&mut CodeAssembler), i: usize) -> f64 {
    use x86jit_core::f80::F80;
    let out = Vector::asm(build).interpret();
    f64::from_bits(F80::from_bytes(&out.cpu.st[i]).to_f64())
}

/// task-206: the x87 transcendentals are now lifted (f64-precision). This upgrades the
/// old tripwire into a real check that the INTERPRETER executes each op with the correct
/// stack effect and produces a result within a tight ULP bound of the `f64` libm
/// reference (the interp computes via libm, so this is ~0 ULP; the bound guards the
/// lift/stack wiring — a wrong top-of-stack, pop count, or push would blow it wide open).
#[test]
fn x87_transcendentals_interp_within_ulp_of_libm() {
    const BOUND: u64 = 2;

    // Single-operand ops leaving the result in ST(0): (name, input, op, libm reference).
    type Case = (&'static str, f64, fn(&mut CodeAssembler), f64);
    let cases: &[Case] = &[
        ("fsin(0.7)", 0.7, |a| a.fsin().unwrap(), 0.7_f64.sin()),
        ("fcos(0.7)", 0.7, |a| a.fcos().unwrap(), 0.7_f64.cos()),
        (
            "f2xm1(0.3)",
            0.3,
            |a| a.f2xm1().unwrap(),
            0.3_f64.exp2() - 1.0,
        ),
    ];
    for &(name, input, op, want) in cases {
        let got = interp_st_f64(
            move |a| {
                push_f64(a, input.to_bits());
                op(a);
                a.hlt().unwrap();
            },
            0,
        );
        assert!(
            transcendental_ulp_diff(got, want) <= BOUND,
            "{name}: interp {got:.20} vs libm {want:.20} ({} ULP > {BOUND})",
            transcendental_ulp_diff(got, want)
        );
    }

    // fptan: ST(0)=1.0 (pushed), tan(input) lands in ST(1).
    let got = interp_st_f64(
        |a| {
            push_f64(a, 0.6_f64.to_bits());
            a.fptan().unwrap();
            a.hlt().unwrap();
        },
        1,
    );
    assert!(
        transcendental_ulp_diff(got, 0.6_f64.tan()) <= BOUND,
        "fptan(0.6) ST1: interp {got:.20} vs libm {:.20}",
        0.6_f64.tan()
    );

    // fsincos: ST(0)=cos, ST(1)=sin.
    let cos = interp_st_f64(
        |a| {
            push_f64(a, 0.5_f64.to_bits());
            a.fsincos().unwrap();
            a.hlt().unwrap();
        },
        0,
    );
    let sin = interp_st_f64(
        |a| {
            push_f64(a, 0.5_f64.to_bits());
            a.fsincos().unwrap();
            a.hlt().unwrap();
        },
        1,
    );
    assert!(
        transcendental_ulp_diff(cos, 0.5_f64.cos()) <= BOUND,
        "fsincos cos"
    );
    assert!(
        transcendental_ulp_diff(sin, 0.5_f64.sin()) <= BOUND,
        "fsincos sin"
    );

    // fpatan: ST0 = atan(ST1/ST0). Load y=1 then x=2 => atan(1/2).
    let got = interp_st_f64(
        |a| {
            push_f64(a, 1.0_f64.to_bits()); // ST1 = y
            push_f64(a, 2.0_f64.to_bits()); // ST0 = x
            a.fpatan().unwrap();
            a.hlt().unwrap();
        },
        0,
    );
    assert!(
        transcendental_ulp_diff(got, 1.0_f64.atan2(2.0)) <= BOUND,
        "fpatan(1,2): interp {got:.20} vs libm {:.20}",
        1.0_f64.atan2(2.0)
    );

    // fyl2x: ST1*log2(ST0). y=3, x=8 => 3*log2(8) = 9.
    let got = interp_st_f64(
        |a| {
            push_f64(a, 3.0_f64.to_bits()); // ST1 = y
            push_f64(a, 8.0_f64.to_bits()); // ST0 = x
            a.fyl2x().unwrap();
            a.hlt().unwrap();
        },
        0,
    );
    assert!(
        transcendental_ulp_diff(got, 3.0 * 8.0_f64.log2()) <= BOUND,
        "fyl2x(3,8): interp {got:.20} vs libm {:.20}",
        3.0 * 8.0_f64.log2()
    );

    // fyl2xp1: ST1*log2(1+ST0). y=2, x=0.25 => 2*log2(1.25).
    let got = interp_st_f64(
        |a| {
            push_f64(a, 2.0_f64.to_bits()); // ST1 = y
            push_f64(a, 0.25_f64.to_bits()); // ST0 = x
            a.fyl2xp1().unwrap();
            a.hlt().unwrap();
        },
        0,
    );
    let want = 2.0 * (0.25_f64.ln_1p() / std::f64::consts::LN_2);
    assert!(
        transcendental_ulp_diff(got, want) <= BOUND,
        "fyl2xp1(2,0.25): interp {got:.20} vs libm {want:.20}",
    );
}

/// Assemble `fld1` then the given transcendental (operating on ST0 = 1.0).
fn transcendental_body(op: fn(&mut CodeAssembler)) -> impl FnOnce(&mut CodeAssembler) {
    move |a: &mut CodeAssembler| {
        a.fld1().unwrap();
        op(a);
        a.hlt().unwrap();
    }
}

/// AC#3 (harness guard): validate the NEW x87-stack capture on transcendentals.
///
/// Unicorn's QEMU-based x87 transcendentals are NOT bit-accurate to real Intel
/// hardware (QEMU computes them in host `long double`/`double`, not with the 68-bit
/// internal precision + range reduction of a physical FPU), so a bit-exact compare of
/// Unicorn's ST(0) is meaningless as a hardware oracle. And our interpreter doesn't
/// implement them at all (see [`x87_transcendentals_unimplemented_in_interp`]). So
/// rather than a false-precise bit compare, this test exercises the harness's new
/// ST(0) read-back through the Unicorn oracle and asserts the captured result rounds
/// to within a DOCUMENTED, small ULP bound of the Rust `f64` libm reference. This is
/// a meaningful regression guard on the capture path (a broken ST read-back, wrong
/// top-of-stack mapping, or byte order would blow the bound wide open) without
/// pretending to hardware-exact transcendental parity.
///
/// Bound: 4 ULP on the f64 result. sin/cos/2^x-1 of these inputs are well-conditioned;
/// QEMU vs libm differ by ≤ a couple ULP after the f80→f64 round, and 4 ULP leaves
/// margin without letting a genuine capture bug through (a mis-read ST(0) is off by
/// millions of ULP or is NaN).
#[cfg(feature = "unicorn")]
#[test]
fn x87_transcendentals_unicorn_within_ulp_of_libm() {
    use x86jit_core::f80::F80;

    /// f64 ULP distance between two finite values.
    fn ulp_diff(a: f64, b: f64) -> u64 {
        // Monotonic mapping of f64 bits to a sortable integer, then |difference|.
        fn key(x: f64) -> i64 {
            let b = x.to_bits() as i64;
            if b < 0 {
                i64::MIN - b
            } else {
                b
            }
        }
        key(a).abs_diff(key(b))
    }

    /// Read Unicorn's ST(0) after running `fld1; <op>; hlt`, rounded to f64.
    fn unicorn_st0(op: fn(&mut CodeAssembler)) -> f64 {
        let out = Vector::asm(transcendental_body(op)).unicorn();
        f64::from_bits(F80::from_bytes(&out.cpu.st[0]).to_f64())
    }

    const MAX_ULP: u64 = 4;

    // fsin: ST0 = sin(1.0)
    let got = unicorn_st0(|a| a.fsin().unwrap());
    let want = 1.0_f64.sin();
    assert!(
        ulp_diff(got, want) <= MAX_ULP,
        "fsin(1.0): unicorn {got:.20} vs libm {want:.20} ({} ULP > {MAX_ULP})",
        ulp_diff(got, want)
    );

    // fcos: ST0 = cos(1.0)
    let got = unicorn_st0(|a| a.fcos().unwrap());
    let want = 1.0_f64.cos();
    assert!(
        ulp_diff(got, want) <= MAX_ULP,
        "fcos(1.0): unicorn {got:.20} vs libm {want:.20} ({} ULP > {MAX_ULP})",
        ulp_diff(got, want)
    );

    // f2xm1: ST0 = 2^1 - 1 = 1.0 (input must be in [-1, 1]; 1.0 is the boundary).
    let got = unicorn_st0(|a| a.f2xm1().unwrap());
    let want = 2.0_f64.powf(1.0) - 1.0;
    assert!(
        ulp_diff(got, want) <= MAX_ULP,
        "f2xm1(1.0): unicorn {got:.20} vs libm {want:.20} ({} ULP > {MAX_ULP})",
        ulp_diff(got, want)
    );

    // fpatan: ST0 = atan(ST1/ST0). Load 1.0 then 1.0 => atan(1/1) = pi/4.
    let out = Vector::asm(|a| {
        a.fld1().unwrap(); // ST1 (denominator after the next push)
        a.fld1().unwrap(); // ST0 (numerator... fpatan computes atan(ST1/ST0))
        a.fpatan().unwrap(); // ST0 = atan(ST1/ST0) = atan(1) = pi/4, pops one
        a.hlt().unwrap();
    })
    .unicorn();
    let got = f64::from_bits(F80::from_bytes(&out.cpu.st[0]).to_f64());
    let want = 1.0_f64.atan2(1.0);
    assert!(
        ulp_diff(got, want) <= MAX_ULP,
        "fpatan(1,1): unicorn {got:.20} vs libm {want:.20} ({} ULP > {MAX_ULP})",
        ulp_diff(got, want)
    );
}

/// task-208: MMX↔XMM bridge `movq2dq`/`movdq2q` + `emms`. MMX aliases the low 64 bits of
/// the physical x87 registers, so a `movdq2q` (XMM→MMX) then `movq2dq` (MMX→XMM) round-trip
/// must reproduce the low 64 bits, and the aliased register's mantissa must match the real
/// FPU. Validated against Unicorn, comparing the XMM round-trip results exactly and the
/// MMX/x87 mantissa (low 64) exactly. The x87 *exponent* tag bytes (79:64) are excluded:
/// Intel sets them all-ones on an MMX write, but Unicorn/QEMU leaves them 0 — a known
/// Unicorn inaccuracy (like its x87 transcendentals), and architecturally irrelevant to the
/// bridge (movq2dq reads only the mantissa).
#[cfg(feature = "unicorn")]
#[test]
fn mmx_bridge_matches_unicorn() {
    let build = |a: &mut CodeAssembler| {
        // Two distinct 64-bit MMX payloads with non-trivial high bits (a lossy F80
        // round-trip would corrupt them), moved XMM→MMX then MMX→XMM, then emms.
        a.mov(rax, 0x7ff8_1234_5678_9abcu64).unwrap();
        a.movq(xmm0, rax).unwrap();
        a.mov(rcx, 0x0123_dead_beef_cafeu64).unwrap();
        a.movq(xmm1, rcx).unwrap();
        a.movdq2q(mm0, xmm0).unwrap(); // xmm0.lo -> mm0 (= fpr[0])
        a.movdq2q(mm3, xmm1).unwrap(); // xmm1.lo -> mm3 (= fpr[3])
        a.movq2dq(xmm5, mm0).unwrap(); // mm0 -> xmm5 (upper zeroed)
        a.movq2dq(xmm6, mm3).unwrap(); // mm3 -> xmm6
        a.emms().unwrap();
        a.hlt().unwrap();
    };
    let interp = Vector::asm(build).interpret();
    let unicorn = Vector::asm(build).unicorn();
    // XMM round-trip: exact.
    assert_eq!(
        interp.cpu.xmm[5], unicorn.cpu.xmm[5],
        "movq2dq mm0 round-trip"
    );
    assert_eq!(
        interp.cpu.xmm[6], unicorn.cpu.xmm[6],
        "movq2dq mm3 round-trip"
    );
    assert_eq!(interp.cpu.xmm[5] as u64, 0x7ff8_1234_5678_9abc, "mm0 low64");
    assert_eq!(interp.cpu.xmm[6] as u64, 0x0123_dead_beef_cafe, "mm3 low64");
    // MMX/x87 mantissa (low 64 of the physical register): exact vs Unicorn. TOP-relative
    // rotation is identity here (both leave TOP at 0), so st[i] == fpr[i].
    for i in [0usize, 3] {
        let im = u64::from_le_bytes(interp.cpu.st[i][0..8].try_into().unwrap());
        let un = u64::from_le_bytes(unicorn.cpu.st[i][0..8].try_into().unwrap());
        assert_eq!(im, un, "fpr[{i}] mantissa matches Unicorn");
    }
}

/// task-212: selecting `X87Precision::Extended` routes the x87 transcendentals through
/// the full-80-bit F80 path. Runs the same `fsin` snippet under both modes on the
/// interpreter and asserts: (1) both round to the correct f64 (within 1 ULP of libm), and
/// (2) their raw 80-bit ST(0) bytes DIFFER — proving the Extended path is actually taken
/// end-to-end (a wrong or ignored precision flag would make them identical).
#[test]
fn x87_extended_precision_selectable() {
    use x86jit_core::f80::F80;
    use x86jit_core::X87Precision;

    for x in [0.7f64, 1.3, 2.5] {
        let bits = x.to_bits();
        let v = Vector::asm(move |a: &mut CodeAssembler| {
            a.mov(rax, bits).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.fld(qword_ptr(SCRATCH)).unwrap();
            a.fsin().unwrap();
            a.hlt().unwrap();
        });
        let fast = v.interpret_x87(X87Precision::Fast);
        let ext = v.interpret_x87(X87Precision::Extended);
        let ff = f64::from_bits(F80::from_bytes(&fast.cpu.st[0]).to_f64());
        let ef = f64::from_bits(F80::from_bytes(&ext.cpu.st[0]).to_f64());
        assert!(transcendental_ulp_diff(ff, x.sin()) <= 1, "fast sin({x})");
        assert!(transcendental_ulp_diff(ef, x.sin()) <= 1, "ext sin({x})");
        assert_ne!(
            fast.cpu.st[0], ext.cpu.st[0],
            "Extended fsin({x}) must differ from Fast in the low 80-bit mantissa"
        );
    }
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

#[test]
fn bt_mem_reg_bit_string_matches_unicorn() {
    // A *register* index against a *memory* operand is a signed bit-string offset,
    // NOT masked to the operand width: the addressed byte is base + (index >> 3)
    // and the bit is index & 7. Indices >= word width and negative indices reach
    // beyond the base word — the case the lifter previously masked wrongly.
    diff(
        bt_bit_string_body,
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

/// bt/bts/btr/btc [mem], reg with indices that leave the base word: index 64 hits
/// the next qword's bit 0, 129 hits bit 1 two qwords up, and a negative index
/// reaches a lower byte. CF captured per-op; modified bytes read back.
fn bt_bit_string_body(a: &mut CodeAssembler) {
    a.mov(qword_ptr(SCRATCH), 0i32).unwrap();
    a.mov(qword_ptr(SCRATCH + 8), 0i32).unwrap();
    a.mov(qword_ptr(SCRATCH + 16), 0i32).unwrap();

    // index 64 -> byte SCRATCH+8, bit 0. Sets it; CF = old bit = 0.
    a.mov(rcx, 64i64).unwrap();
    a.bts(qword_ptr(SCRATCH), rcx).unwrap();
    a.setb(r8b).unwrap();
    a.mov(r9, qword_ptr(SCRATCH + 8)).unwrap(); // expect 1

    // index 129 -> byte SCRATCH+16, bit 1. Toggles it; CF = old bit = 0.
    a.mov(rdx, 129i64).unwrap();
    a.btc(qword_ptr(SCRATCH), rdx).unwrap();
    a.setb(r10b).unwrap();
    a.mov(r11, qword_ptr(SCRATCH + 16)).unwrap(); // expect 2

    // Re-read the bit just set at SCRATCH+8:0 via index 64 -> CF = 1.
    a.bt(qword_ptr(SCRATCH), rcx).unwrap();
    a.setb(r12b).unwrap();

    // Negative index: base SCRATCH+16, index -128 -> byte SCRATCH, bit 0.
    a.mov(qword_ptr(SCRATCH), 1i32).unwrap(); // bit 0 set
    a.mov(rax, -128i64).unwrap();
    a.bt(qword_ptr(SCRATCH + 16), rax).unwrap(); // CF = 1
    a.setb(r13b).unwrap();

    a.hlt().unwrap();
}

#[test]
fn lea_ignores_segment_base_matches_unicorn() {
    // `lea` computes the address offset and must IGNORE the segment base: with a live
    // FS base, `lea rax, fs:[rbx]` is `rax = rbx`, not `rbx + fs_base`. (A memory
    // *access* through fs would add the base; lea does not.)
    diff(
        |a| {
            a.mov(rbx, 0x2000i64).unwrap();
            a.lea(rax, qword_ptr(rbx).fs()).unwrap(); // expect rax = 0x2000
            a.hlt().unwrap();
        },
        |s| s.fs_base = 0x5000, // nonzero, so the old (buggy) add would show as 0x7000
        &[],
    );
}

#[test]
fn addr_size_override_truncates_to_32_bits_matches_unicorn() {
    // A 0x67 address-size override truncates the effective address to 32 bits:
    // `mov eax, [ebx]` with RBX = 0x1_0000_0000 + SCRATCH reads [SCRATCH], not the
    // 64-bit RBX (which is unmapped). iced emits the 0x67 form for a 32-bit base reg.
    diff(
        |a| {
            a.mov(rbx, 0x1_0000_0000u64 + SCRATCH).unwrap();
            a.mov(dword_ptr(SCRATCH), 0x1234i32).unwrap();
            a.mov(eax, dword_ptr(ebx)).unwrap(); // truncates ebx → SCRATCH
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
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

/// A branchless block longer than the lifter's fetch window (`BLOCK_FETCH_WINDOW`,
/// 4 KiB) must split at the last complete instruction and fall through to a
/// continuation block — carrying flags across the cut — not decode the truncated
/// tail as a bogus fault. This is the go-caddy P5-real regression: Go's bignum
/// crypto (`p521Square`) has >4 KiB branchless stretches (task-161). ~2600 two-byte
/// `adc` instructions (>5 KiB) force at least one window cut, and each `adc` reads
/// the carry the previous one set — so a mis-elided carry across the boundary would
/// diverge from Unicorn.
#[test]
fn branchless_block_longer_than_fetch_window() {
    diff(
        |a| {
            a.mov(eax, 0i32).unwrap();
            a.mov(ebx, 1i32).unwrap();
            for _ in 0..2600 {
                a.adc(eax, ebx).unwrap(); // eax += ebx + CF; sets CF for the next one
            }
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

// --- AVX (VEX.128) — task-168.1. Each VEX.128 form must lower to the same state as
// its legacy-SSE equivalent (`vex_eq_sse`); Unicorn can't be the oracle (its QEMU
// build drops VEX.vvvv), but SSE is corpus-validated against it. ---

const A: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
const B: u128 = 0xFF00_FF00_1234_5678_9ABC_DEF0_0011_2233;

fn seed_ab(s: &mut CpuSnapshot) {
    s.xmm[1] = A;
    s.xmm[2] = B;
}

#[test]
fn vex128_vpxor_three_operand() {
    vex_eq_sse(
        |a| {
            a.vpxor(xmm0, xmm1, xmm2).unwrap(); // xmm0 = xmm1 ^ xmm2 (non-destructive)
            a.hlt().unwrap();
        },
        |a| {
            a.movdqa(xmm0, xmm1).unwrap();
            a.pxor(xmm0, xmm2).unwrap();
            a.hlt().unwrap();
        },
        seed_ab,
    );
}

#[test]
fn vex128_vpxor_self_zeroes() {
    vex_eq_sse(
        |a| {
            a.vpxor(xmm3, xmm3, xmm3).unwrap(); // the ubiquitous zeroing idiom
            a.hlt().unwrap();
        },
        |a| {
            a.pxor(xmm3, xmm3).unwrap();
            a.hlt().unwrap();
        },
        |s| s.xmm[3] = A,
    );
}

#[test]
fn vex128_vmovdqu_reg_and_memory() {
    vex_eq_sse(
        |a| {
            a.vmovdqu(xmm5, xmm1).unwrap();
            a.vmovdqu(xmmword_ptr(SCRATCH), xmm5).unwrap();
            a.vmovdqu(xmm6, xmmword_ptr(SCRATCH)).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.movdqu(xmm5, xmm1).unwrap();
            a.movdqu(xmmword_ptr(SCRATCH), xmm5).unwrap();
            a.movdqu(xmm6, xmmword_ptr(SCRATCH)).unwrap();
            a.hlt().unwrap();
        },
        |s| s.xmm[1] = A,
    );
}

#[test]
fn vex128_vpand_vpor_vpandn() {
    vex_eq_sse(
        |a| {
            a.vpand(xmm0, xmm1, xmm2).unwrap();
            a.vpor(xmm3, xmm1, xmm2).unwrap();
            a.vpandn(xmm4, xmm1, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.movdqa(xmm0, xmm1).unwrap();
            a.pand(xmm0, xmm2).unwrap();
            a.movdqa(xmm3, xmm1).unwrap();
            a.por(xmm3, xmm2).unwrap();
            a.movdqa(xmm4, xmm1).unwrap();
            a.pandn(xmm4, xmm2).unwrap();
            a.hlt().unwrap();
        },
        seed_ab,
    );
}

#[test]
fn vex128_vpcmpeqb_and_vpmovmskb() {
    vex_eq_sse(
        |a| {
            a.vpcmpeqb(xmm0, xmm1, xmm2).unwrap();
            a.vpmovmskb(eax, xmm0).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.movdqa(xmm0, xmm1).unwrap();
            a.pcmpeqb(xmm0, xmm2).unwrap();
            a.pmovmskb(eax, xmm0).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[1] = A;
            s.xmm[2] = A ^ 0x00FF_0000_0000_00FF; // equal in most bytes, differ in a few
        },
    );
}

#[test]
fn vex128_vpaddb_vpsubb() {
    vex_eq_sse(
        |a| {
            a.vpaddb(xmm0, xmm1, xmm2).unwrap();
            a.vpsubb(xmm3, xmm1, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.movdqa(xmm0, xmm1).unwrap();
            a.paddb(xmm0, xmm2).unwrap();
            a.movdqa(xmm3, xmm1).unwrap();
            a.psubb(xmm3, xmm2).unwrap();
            a.hlt().unwrap();
        },
        seed_ab,
    );
}

#[test]
fn vex128_vpshufb_three_operand() {
    vex_eq_sse(
        |a| {
            a.vpshufb(xmm0, xmm1, xmm2).unwrap(); // xmm0 = shuffle(xmm1, xmm2)
            a.hlt().unwrap();
        },
        |a| {
            a.movdqa(xmm0, xmm1).unwrap();
            a.pshufb(xmm0, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[1] = A;
            s.xmm[2] = 0x0001_0203_0405_0607_0809_0A0B_0C0D_0E0F;
        },
    );
}

/// task-168.6: `vextractps r/m32, xmm, imm8` (VEX.128.66.0F3A.W0 17) — extract the
/// 32-bit float lane `imm8[1:0]` from an xmm to a GPR32 dst. Unicorn is the oracle
/// (this is a 2-operand VEX form with no `vvvv`, so its QEMU build decodes it fine,
/// unlike the 3-operand forms that need `vex_eq_sse`). Covers all four lanes; the
/// GPR32 write must zero the upper 32 bits of the destination register (per
/// `mov r32` semantics — seeded with all-ones to prove the zero-extend).
#[test]
fn vextractps_reg_dst_all_lanes_match_unicorn() {
    diff(
        |a| {
            a.vextractps(eax, xmm0, 0i32).unwrap(); // lane 0 → bits [31:0]
            a.vextractps(ebx, xmm0, 1i32).unwrap(); // lane 1 → bits [63:32]
            a.vextractps(ecx, xmm0, 2i32).unwrap(); // lane 2 → bits [95:64]
            a.vextractps(edx, xmm0, 3i32).unwrap(); // lane 3 → bits [127:96]
            a.hlt().unwrap();
        },
        |s| {
            // Distinct per-lane dwords so a wrong lane select is observable.
            s.xmm[0] = 0xDDDD_DDDD_CCCC_CCCC_BBBB_BBBB_AAAA_AAAA;
            // Pre-load the dst GPRs with all-ones so the 32-bit write's upper-zeroing shows.
            s.gpr[0] = 0xFFFF_FFFF_FFFF_FFFF; // rax
            s.gpr[3] = 0xFFFF_FFFF_FFFF_FFFF; // rbx
            s.gpr[1] = 0xFFFF_FFFF_FFFF_FFFF; // rcx
            s.gpr[2] = 0xFFFF_FFFF_FFFF_FFFF; // rdx
        },
        &[],
    );
}

/// task-168.6: `vextractps m32, xmm, imm8` — the memory-destination form (the exact
/// shape that walled Celeste boot: `vextractps $0x2,%xmm0,0x2c(%rsp)`). Store each
/// lane to a distinct scratch dword, then read them back into GPRs so the final state
/// diff against Unicorn proves the 4-byte store landed with the right lane.
#[test]
fn vextractps_mem_dst_all_lanes_match_unicorn() {
    diff(
        |a| {
            a.vextractps(dword_ptr(SCRATCH), xmm0, 0i32).unwrap();
            a.vextractps(dword_ptr(SCRATCH + 4), xmm0, 1i32).unwrap();
            a.vextractps(dword_ptr(SCRATCH + 8), xmm0, 2i32).unwrap();
            a.vextractps(dword_ptr(SCRATCH + 12), xmm0, 3i32).unwrap();
            // Read the stored dwords back so they land in observable GPR state.
            a.mov(eax, dword_ptr(SCRATCH)).unwrap();
            a.mov(ebx, dword_ptr(SCRATCH + 4)).unwrap();
            a.mov(ecx, dword_ptr(SCRATCH + 8)).unwrap();
            a.mov(edx, dword_ptr(SCRATCH + 12)).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = 0xDDDD_DDDD_CCCC_CCCC_BBBB_BBBB_AAAA_AAAA;
        },
        &[],
    );
}

/// task-255: VEX.128 `vinsertps xmm1, xmm2, xmm3, imm8` (3-operand). Unicorn's QEMU build
/// mis-decodes 3-operand VEX (drops `vvvv`), so validate the VEX lowering against the
/// equivalent legacy-SSE 2-operand `insertps` (which the corpus validates against Unicorn).
/// The SSE form is `dst==src1`, so the equivalent SSE sequence copies the merge base (op1)
/// into the dst first. Covers several imm8: a src-lane select, a non-zero zmask, and a
/// different count_d (dst lane). The dst registers differ from both sources so the distinct
/// merge base is exercised.
#[test]
fn vinsertps_reg_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            // src lane 1 → dst lane 0, no zeroing (imm=0x40).
            a.vinsertps(xmm5, xmm0, xmm1, 0x40).unwrap();
            // src lane 2 → dst lane 2, zero dwords 1 & 3 (imm=0xAA).
            a.vinsertps(xmm6, xmm0, xmm1, 0xAA).unwrap();
            // src lane 0 → dst lane 3, zero ALL dwords (imm=0x3F → count_d=3, zmask=0xF).
            a.vinsertps(xmm7, xmm2, xmm3, 0x3F).unwrap();
        },
        |a| {
            a.movaps(xmm5, xmm0).unwrap();
            a.insertps(xmm5, xmm1, 0x40).unwrap();
            a.movaps(xmm6, xmm0).unwrap();
            a.insertps(xmm6, xmm1, 0xAA).unwrap();
            a.movaps(xmm7, xmm2).unwrap();
            a.insertps(xmm7, xmm3, 0x3F).unwrap();
        },
        |c| {
            c.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
            c.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
            c.xmm[2] = 0x5555_5555_6666_6666_7777_7777_8888_8888;
            c.xmm[3] = 0x9999_9999_0A0A_0A0A_0B0B_0B0B_0C0C_0C0C;
        },
    );
}

/// task-255: the m32 form `vinsertps xmm1, xmm2, m32, imm8` — the inserted dword comes from
/// memory (imm[7:6] ignored). Stage a dword in scratch, then insert it with a dst-lane +
/// zmask imm. Validated against the equivalent SSE `insertps xmm, m32, imm8` (dst==src1).
#[test]
fn vinsertps_mem_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.mov(dword_ptr(SCRATCH), 0x4048_F5C3u32 as i32).unwrap(); // 3.14f bit pattern
                                                                       // m32 → dst lane 1, zero dword 3 (imm=0x18 → count_d=1, zmask=0b1000).
            a.vinsertps(xmm6, xmm0, dword_ptr(SCRATCH), 0x18).unwrap();
        },
        |a| {
            a.mov(dword_ptr(SCRATCH), 0x4048_F5C3u32 as i32).unwrap();
            a.movaps(xmm6, xmm0).unwrap();
            a.insertps(xmm6, dword_ptr(SCRATCH), 0x18).unwrap();
        },
        |c| {
            c.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
        },
    );
}

/// task-255: the exact wild encoding that walled Celeste — `c4 e3 79 21 d1 10` =
/// `vinsertps xmm2, xmm0, xmm1, 0x10` (dst=xmm2, vvvv=xmm0, rm=xmm1, imm=0x10 → src lane 0
/// → dst lane 1, no zeroing). Assert the raw bytes decode+run to a hand-computed result and
/// that VEX.128 zeroes bits 255:128 (seed a dirty ymm_hi).
#[test]
fn vinsertps_celeste_wild_bytes() {
    // Assemble and confirm the encoding matches the faulting bytes exactly.
    let mut asm = iced_x86::code_asm::CodeAssembler::new(64).unwrap();
    asm.vinsertps(xmm2, xmm0, xmm1, 0x10i32).unwrap();
    let bytes = asm.assemble(0).unwrap();
    assert_eq!(
        bytes,
        vec![0xc4, 0xe3, 0x79, 0x21, 0xd1, 0x10],
        "encoding must be the Celeste wall bytes c4 e3 79 21 d1 10"
    );

    let o = Vector::asm(|a| {
        a.vinsertps(xmm2, xmm0, xmm1, 0x10i32).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        s.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444; // merge base (vvvv)
        s.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD; // source (rm)
        s.ymm_hi[2] = u128::MAX; // stale upper that VEX.128 must clear
    })
    .interpret();
    // imm=0x10: count_s=0 (src lane 0 = xmm1 dword0 = 0xDDDD_DDDD), count_d=1 (dst lane 1),
    // zmask=0. Result = merge base xmm0 with dword1 replaced by 0xDDDD_DDDD.
    assert_eq!(o.cpu.xmm[2], 0x1111_1111_2222_2222_DDDD_DDDD_4444_4444);
    assert_eq!(o.cpu.ymm_hi[2], 0, "VEX.128 zeroes bits 255:128");
}

/// task-259: the exact encoding that walled Celeste's libfmod — `c4 e2 3d 2e 11` =
/// `vmaskmovps ymmword ptr [rcx], ymm8, ymm2` (VEX.256.66.0F38.W0 2E /r): mask = ymm8
/// (per-32-bit-lane sign bit), data = ymm2, dest = [rcx]. Assert the raw bytes decode+run
/// with no `UnknownInstruction`, and that masked-off lanes leave the (zeroed) store target
/// untouched while active lanes commit — read back via `vmovdqu`.
#[test]
fn vmaskmovps_celeste_wild_bytes() {
    let mut asm = iced_x86::code_asm::CodeAssembler::new(64).unwrap();
    asm.vmaskmovps(ymmword_ptr(rcx), ymm8, ymm2).unwrap();
    let bytes = asm.assemble(0).unwrap();
    assert_eq!(
        bytes,
        vec![0xc4, 0xe2, 0x3d, 0x2e, 0x11],
        "encoding must be the Celeste blocker bytes c4 e2 3d 2e 11"
    );

    let o = Vector::asm(|a| {
        a.mov(rcx, SCRATCH).unwrap();
        a.vmaskmovps(ymmword_ptr(rcx), ymm8, ymm2).unwrap();
        a.vmovdqu(ymm3, ymmword_ptr(rcx)).unwrap(); // read the store result back
        a.hlt().unwrap();
    })
    .init(|s| {
        // ymm2 data: per-lane dword tags 0x11..0x88.
        s.xmm[2] = 0x44444444_33333333_22222222_11111111;
        s.ymm_hi[2] = 0x88888888_77777777_66666666_55555555;
        // ymm8 mask: lanes 0,2,4,6 active (sign bit set), 1,3,5,7 masked off.
        s.xmm[8] = 0x00000000_80000000_00000000_80000000;
        s.ymm_hi[8] = 0x00000000_80000000_00000000_80000000;
        s.ymm_hi[3] = u128::MAX; // observe the VEX.256 full read-back
    })
    .interpret();
    // Masked-off lanes stayed 0 (zeroed scratch), active lanes hold the data tags.
    assert_eq!(o.cpu.xmm[3], 0x00000000_33333333_00000000_11111111);
    assert_eq!(o.cpu.ymm_hi[3], 0x00000000_77777777_00000000_55555555);
}

// --- task-257: VEX float-op sweep — vsqrtp{s,d}, vrsqrtss/vrcpss (scalar, m32) +
// vrsqrtps/vrcpps (packed), vshufps/vshufpd, SSE float unpck bases + VEX vunpck*. ---

/// task-257: the exact wild encoding that walled Celeste — `c5 fa 52 d0` =
/// `vrsqrtss xmm2, xmm0, xmm0` (VEX.128.F3.0F.WIG 52 /r, 3-operand). Assert the raw bytes
/// decode+run (no `UnknownInstruction`) to the exact-IEEE reciprocal-sqrt of the low element
/// (`1.0/sqrt(1.0) == 1.0`), the upper element comes from the merge base (op1 = xmm0), and
/// VEX.128 zeroes bits 255:128 (seed a dirty ymm_hi).
#[test]
fn vrsqrtss_celeste_wild_bytes() {
    // Assemble and confirm the encoding matches the faulting bytes exactly.
    let mut asm = iced_x86::code_asm::CodeAssembler::new(64).unwrap();
    asm.vrsqrtss(xmm2, xmm0, xmm0).unwrap();
    let bytes = asm.assemble(0).unwrap();
    assert_eq!(
        bytes,
        vec![0xc5, 0xfa, 0x52, 0xd0],
        "encoding must be the Celeste wall bytes c5 fa 52 d0"
    );

    let o = Vector::asm(|a| {
        a.vrsqrtss(xmm2, xmm0, xmm0).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        // xmm0 low = 1.0f32 (0x3f800000); upper dwords 2.0/3.0/4.0 exercise the merge base.
        s.xmm[0] = 0x4080_0000_4040_0000_4000_0000_3f80_0000;
        s.ymm_hi[2] = u128::MAX; // stale upper that VEX.128 must clear
    })
    .interpret();
    assert_eq!(
        o.exit,
        x86jit_tests::vector::ExitKind::Hlt,
        "vrsqrtss must decode (no UnknownInstruction)"
    );
    // Low lane = 1.0/sqrt(1.0) = 1.0 (bits 0x3f800000); upper lanes from op1 (xmm0).
    assert_eq!(
        o.cpu.xmm[2], 0x4080_0000_4040_0000_4000_0000_3f80_0000,
        "low lane = 1.0/sqrt(1.0); upper from merge base xmm0"
    );
    assert_eq!(o.cpu.ymm_hi[2], 0, "VEX.128 zeroes bits 255:128");
}

/// task-257: VEX packed sqrt `vsqrtps`/`vsqrtpd` (2-operand, no vvvv). Validated against the
/// legacy-SSE `sqrtps`/`sqrtpd` (dst = op(src), so the SSE equivalent is `movaps dst,src` +
/// `sqrtp*`). Covers the register and m128 source forms.
#[test]
fn vsqrt_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.vsqrtps(xmm5, xmm0).unwrap();
            a.vsqrtpd(xmm6, xmm1).unwrap();
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm2).unwrap();
            a.vsqrtps(xmm7, xmmword_ptr(rax)).unwrap(); // m128 source
        },
        |a| {
            a.movaps(xmm5, xmm0).unwrap();
            a.sqrtps(xmm5, xmm5).unwrap();
            a.movaps(xmm6, xmm1).unwrap();
            a.sqrtpd(xmm6, xmm6).unwrap();
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm2).unwrap();
            a.movaps(xmm7, xmm2).unwrap();
            a.sqrtps(xmm7, xmm7).unwrap();
        },
        |c| {
            // 4.0, 9.0, 16.0, 25.0 (f32) → 2,3,4,5.
            c.xmm[0] = 0x41c8_0000_4180_0000_4110_0000_4080_0000;
            // 4.0, 9.0 (f64) → 2, 3.
            c.xmm[1] = 0x4022_0000_0000_0000_4010_0000_0000_0000;
            c.xmm[2] = 0x41c8_0000_4180_0000_4110_0000_4080_0000;
        },
    );
}

/// task-257: VEX 3-operand shuffles `vshufps`/`vshufpd` — distinct merge base (vvvv). The SSE
/// form is `dst==src1`, so the equivalent SSE sequence copies the merge base (op1) into dst
/// first. Lanes 0,1 come from op1, lanes 2,3 from op2, per the imm8. Covers reg + m128 src2.
#[test]
fn vshuf_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.vshufps(xmm5, xmm0, xmm1, 0x1Bi32).unwrap(); // reverse dword select
            a.vshufpd(xmm6, xmm0, xmm1, 0x01i32).unwrap(); // lane0 from a[1], lane1 from b[0]
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.movdqu(xmm2, xmmword_ptr(rax)).unwrap(); // keep xmm2 in step with the SSE ref
            a.vshufps(xmm7, xmm0, xmmword_ptr(rax), 0xE4i32).unwrap(); // m128 src2, identity
        },
        |a| {
            a.movaps(xmm5, xmm0).unwrap();
            a.shufps(xmm5, xmm1, 0x1Bi32).unwrap();
            a.movaps(xmm6, xmm0).unwrap();
            a.shufpd(xmm6, xmm1, 0x01i32).unwrap();
            // Legacy SSE `shufps` has no lifted m128 form; load the operand into a register
            // first, then use the register `shufps` to build the same reference result.
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.movdqu(xmm2, xmmword_ptr(rax)).unwrap();
            a.movaps(xmm7, xmm0).unwrap();
            a.shufps(xmm7, xmm2, 0xE4i32).unwrap();
        },
        |c| {
            c.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
            c.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
        },
    );
}

/// task-257: SSE float unpacks `unpcklps`/`unpckhps`/`unpcklpd`/`unpckhpd` + their VEX
/// 3-operand forms `vunpck*`. The SSE forms are `dst==src1`; the VEX forms take a distinct
/// merge base (vvvv). Validated by copying the merge base into dst for the SSE equivalent.
/// Covers reg + m128 src2 (the VEX m128 path pre-copies op1 into dst).
#[test]
fn vunpck_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.vunpcklps(xmm5, xmm0, xmm1).unwrap();
            a.vunpckhps(xmm6, xmm0, xmm1).unwrap();
            a.vunpcklpd(xmm7, xmm0, xmm1).unwrap();
            a.vunpckhpd(xmm8, xmm0, xmm1).unwrap();
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.vunpcklps(xmm9, xmm0, xmmword_ptr(rax)).unwrap(); // m128 src2
        },
        |a| {
            a.movaps(xmm5, xmm0).unwrap();
            a.unpcklps(xmm5, xmm1).unwrap();
            a.movaps(xmm6, xmm0).unwrap();
            a.unpckhps(xmm6, xmm1).unwrap();
            a.movaps(xmm7, xmm0).unwrap();
            a.unpcklpd(xmm7, xmm1).unwrap();
            a.movaps(xmm8, xmm0).unwrap();
            a.unpckhpd(xmm8, xmm1).unwrap();
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.movaps(xmm9, xmm0).unwrap();
            a.unpcklps(xmm9, xmmword_ptr(rax)).unwrap();
        },
        |c| {
            c.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
            c.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
        },
    );
}

// --- task-258: VEX.256 (YMM) float sweep — vcvt{dq2ps,ps2dq,tps2dq}, vadd/sub/mul/div/
// min/max{ps,pd}, vsqrt{ps,pd}, vshuf{ps,pd}, vunpck{l,h}p{s,d} — the 256-bit forms that
// mechanically extend the VEX.128 float ops to the upper 128-bit lane (ymm_hi). Celeste
// (Mono+FNA) faulted `c5 fc 5b c0` = vcvtdq2ps ymm0, ymm0. The scalar/pd width-changing
// converts stay 128-bit-only (deferred). ---

/// task-258: the exact wild encoding that walled Celeste — `c5 fc 5b c0` =
/// `vcvtdq2ps ymm0, ymm0` (2-byte VEX C5, VEX.256.0F.WIG 5B /r, L=1 → YMM). Assert the raw
/// bytes decode+run (no `UnknownInstruction`) and convert all 8 packed int32 lanes (both
/// 128-bit halves) to float. VEX.256 writes the WHOLE 256-bit register (no upper-zeroing).
#[test]
fn vcvtdq2ps_ymm_celeste_wild_bytes() {
    let mut asm = iced_x86::code_asm::CodeAssembler::new(64).unwrap();
    asm.vcvtdq2ps(ymm0, ymm0).unwrap();
    let bytes = asm.assemble(0).unwrap();
    assert_eq!(
        bytes,
        vec![0xc5, 0xfc, 0x5b, 0xc0],
        "encoding must be the Celeste wall bytes c5 fc 5b c0"
    );

    let o = Vector::asm(|a| {
        a.vcvtdq2ps(ymm0, ymm0).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        // Low half: i32 lanes 4,3,2,1. High half: i32 lanes 8,7,6,5.
        s.xmm[0] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
        s.ymm_hi[0] = 0x0000_0008_0000_0007_0000_0006_0000_0005;
    })
    .interpret();
    assert_eq!(
        o.exit,
        x86jit_tests::vector::ExitKind::Hlt,
        "vcvtdq2ps ymm must decode (no UnknownInstruction)"
    );
    // f32 bits: 1.0=0x3f800000, 2.0=0x40000000, 3.0=0x40400000, 4.0=0x40800000,
    // 5.0=0x40a00000, 6.0=0x40c00000, 7.0=0x40e00000, 8.0=0x41000000.
    assert_eq!(
        o.cpu.xmm[0], 0x4080_0000_4040_0000_4000_0000_3f80_0000,
        "low half: int32 {{4,3,2,1}} -> f32"
    );
    assert_eq!(
        o.cpu.ymm_hi[0], 0x4100_0000_40e0_0000_40c0_0000_40a0_0000,
        "high half: int32 {{8,7,6,5}} -> f32 (whole YMM written)"
    );
}

/// task-258: 256-bit lane-preserving converts `vcvtdq2ps`/`vcvtps2dq`/`vcvttps2dq ymm`.
/// SSE-per-half reference: split each ymm with `vextractf128`, convert each 128-bit half with
/// the legacy SSE op, recombine with `vinsertf128`. Covers register + 32-byte memory source.
#[test]
fn vcvt_ymm_eq_sse() {
    vex256_eq_sse(
        |a| {
            a.vcvtdq2ps(ymm2, ymm0).unwrap();
            a.vcvtps2dq(ymm3, ymm1).unwrap();
            a.vcvttps2dq(ymm4, ymm1).unwrap();
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm0).unwrap();
            a.vcvtdq2ps(ymm5, ymmword_ptr(rax)).unwrap(); // 32-byte memory source
        },
        |a| {
            // dq2ps(ymm0): halves ymm0.lo, ymm0.hi.
            a.vextractf128(xmm10, ymm0, 0).unwrap();
            a.vextractf128(xmm11, ymm0, 1).unwrap();
            a.cvtdq2ps(xmm10, xmm10).unwrap();
            a.cvtdq2ps(xmm11, xmm11).unwrap();
            a.vinsertf128(ymm2, ymm2, xmm10, 0).unwrap();
            a.vinsertf128(ymm2, ymm2, xmm11, 1).unwrap();
            // ps2dq(ymm1).
            a.vextractf128(xmm10, ymm1, 0).unwrap();
            a.vextractf128(xmm11, ymm1, 1).unwrap();
            a.cvtps2dq(xmm10, xmm10).unwrap();
            a.cvtps2dq(xmm11, xmm11).unwrap();
            a.vinsertf128(ymm3, ymm3, xmm10, 0).unwrap();
            a.vinsertf128(ymm3, ymm3, xmm11, 1).unwrap();
            // tps2dq(ymm1).
            a.vextractf128(xmm10, ymm1, 0).unwrap();
            a.vextractf128(xmm11, ymm1, 1).unwrap();
            a.cvttps2dq(xmm10, xmm10).unwrap();
            a.cvttps2dq(xmm11, xmm11).unwrap();
            a.vinsertf128(ymm4, ymm4, xmm10, 0).unwrap();
            a.vinsertf128(ymm4, ymm4, xmm11, 1).unwrap();
            // dq2ps from memory == dq2ps(ymm0); keep xmm-move parity with the VEX arm.
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm0).unwrap();
            a.vextractf128(xmm10, ymm0, 0).unwrap();
            a.vextractf128(xmm11, ymm0, 1).unwrap();
            a.cvtdq2ps(xmm10, xmm10).unwrap();
            a.cvtdq2ps(xmm11, xmm11).unwrap();
            a.vinsertf128(ymm5, ymm5, xmm10, 0).unwrap();
            a.vinsertf128(ymm5, ymm5, xmm11, 1).unwrap();
        },
        |c| {
            // ymm0 int32 lanes; ymm1 f32 lanes (whole-number so ps2dq==tps2dq exactly).
            c.xmm[0] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
            c.ymm_hi[0] = 0xFFFF_FFF8_0000_0007_0000_0006_0000_0005; // includes a negative lane
            c.xmm[1] = 0x40800000_40400000_40000000_3f800000; // f32 4,3,2,1
            c.ymm_hi[1] = 0xc1000000_40e00000_40c00000_40a00000; // f32 5,6,7,-8
        },
    );
}

/// task-258: 256-bit packed arithmetic `v{add,sub,mul,div,min,max}{ps,pd} ymm`. SSE-per-half
/// reference. Covers register + 32-byte memory src2 (add).
#[test]
fn varith_ymm_eq_sse() {
    vex256_eq_sse(
        |a| {
            a.vaddps(ymm2, ymm0, ymm1).unwrap();
            a.vsubpd(ymm3, ymm0, ymm1).unwrap();
            a.vmulps(ymm4, ymm0, ymm1).unwrap();
            a.vdivpd(ymm5, ymm0, ymm1).unwrap();
            a.vminps(ymm6, ymm0, ymm1).unwrap();
            a.vmaxpd(ymm7, ymm0, ymm1).unwrap();
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm1).unwrap();
            a.vaddps(ymm8, ymm0, ymmword_ptr(rax)).unwrap(); // 32-byte memory src2
        },
        |a| {
            sse_ymm_bin(a, 2, 0, 1, BinKind::AddPs);
            sse_ymm_bin(a, 3, 0, 1, BinKind::SubPd);
            sse_ymm_bin(a, 4, 0, 1, BinKind::MulPs);
            sse_ymm_bin(a, 5, 0, 1, BinKind::DivPd);
            sse_ymm_bin(a, 6, 0, 1, BinKind::MinPs);
            sse_ymm_bin(a, 7, 0, 1, BinKind::MaxPd);
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm1).unwrap();
            sse_ymm_bin(a, 8, 0, 1, BinKind::AddPs);
        },
        |c| {
            c.xmm[0] = 0x40800000_40400000_40000000_3f800000; // f32 4,3,2,1
            c.ymm_hi[0] = 0x4020000000000000_4010000000000000; // f64 8.0, 4.0
            c.xmm[1] = 0x40000000_40000000_40000000_40000000; // f32 2,2,2,2
            c.ymm_hi[1] = 0x4000000000000000_4000000000000000; // f64 2.0, 2.0
        },
    );
}

/// task-258: 256-bit packed `vsqrt{ps,pd} ymm`. SSE-per-half reference. Covers reg + m256.
#[test]
fn vsqrt_ymm_eq_sse() {
    vex256_eq_sse(
        |a| {
            a.vsqrtps(ymm2, ymm0).unwrap();
            a.vsqrtpd(ymm3, ymm1).unwrap();
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm0).unwrap();
            a.vsqrtps(ymm4, ymmword_ptr(rax)).unwrap(); // m256 source
        },
        |a| {
            sse_ymm_sqrt(a, 2, 0, false);
            sse_ymm_sqrt(a, 3, 1, true);
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm0).unwrap();
            sse_ymm_sqrt(a, 4, 0, false);
        },
        |c| {
            c.xmm[0] = 0x41c80000_41100000_41100000_40800000; // f32 4,9,9,25
            c.ymm_hi[0] = 0x41200000_41100000_40800000_40000000; // f32 2,4,9,10
            c.xmm[1] = 0x4022000000000000_4010000000000000; // f64 4.0, 9.0
            c.ymm_hi[1] = 0x4090000000000000_4040000000000000; // f64 32.0, 1024.0
        },
    );
}

/// task-258: 256-bit `vshuf{ps,pd} ymm` (per-128-lane) + `vunpck{l,h}p{s,d} ymm`. SSE-per-half
/// reference. vshufpd's imm bits differ per 128-bit half — the reference splits and applies the
/// correct per-half imm. Covers register + 32-byte memory src2.
#[test]
fn vshuf_vunpck_ymm_eq_sse() {
    vex256_eq_sse(
        |a| {
            a.vshufps(ymm2, ymm0, ymm1, 0x1Bi32).unwrap();
            a.vshufpd(ymm3, ymm0, ymm1, 0x09i32).unwrap(); // imm[1:0]=01 (lo), imm[3:2]=10 (hi)
            a.vunpcklps(ymm4, ymm0, ymm1).unwrap();
            a.vunpckhps(ymm5, ymm0, ymm1).unwrap();
            a.vunpcklpd(ymm6, ymm0, ymm1).unwrap();
            a.vunpckhpd(ymm7, ymm0, ymm1).unwrap();
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm1).unwrap();
            a.vshufps(ymm8, ymm0, ymmword_ptr(rax), 0x4Ei32).unwrap(); // m256 src2
        },
        |a| {
            sse_ymm_shufps(a, 2, 0, 1, 0x1B, 0x1B);
            sse_ymm_shufpd(a, 3, 0, 1, 0x01, 0x02); // per-half pd imm: lo=01, hi=10
            sse_ymm_unpck(a, 4, 0, 1, UnpKind::LowPs);
            sse_ymm_unpck(a, 5, 0, 1, UnpKind::HighPs);
            sse_ymm_unpck(a, 6, 0, 1, UnpKind::LowPd);
            sse_ymm_unpck(a, 7, 0, 1, UnpKind::HighPd);
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm1).unwrap();
            sse_ymm_shufps(a, 8, 0, 1, 0x4E, 0x4E);
        },
        |c| {
            c.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
            c.ymm_hi[0] = 0x1010_1010_2020_2020_3030_3030_4040_4040;
            c.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
            c.ymm_hi[1] = 0x0A0A_0A0A_0B0B_0B0B_0C0C_0C0C_0D0D_0D0D;
        },
    );
}

// --- SSE-per-half reference builders for the task-258 differential tests. Each splits the two
// ymm sources into their 128-bit halves (`vextractf128`, trusted), applies the legacy-SSE op to
// each half, and recombines (`vinsertf128`). Scratch xmm10/xmm11 hold the halves; the SSE op is
// done in place on xmm10/xmm12. ---

#[derive(Copy, Clone)]
enum BinKind {
    AddPs,
    SubPd,
    MulPs,
    DivPd,
    MinPs,
    MaxPd,
}

fn sse_ymm_bin(a: &mut CodeAssembler, d: u32, s0: u32, s1: u32, kind: BinKind) {
    let (yd, y0, y1) = (ymm_reg(d), ymm_reg(s0), ymm_reg(s1));
    for lane in 0..2i32 {
        a.vextractf128(xmm10, y0, lane).unwrap();
        a.vextractf128(xmm12, y1, lane).unwrap();
        match kind {
            BinKind::AddPs => a.addps(xmm10, xmm12).unwrap(),
            BinKind::SubPd => a.subpd(xmm10, xmm12).unwrap(),
            BinKind::MulPs => a.mulps(xmm10, xmm12).unwrap(),
            BinKind::DivPd => a.divpd(xmm10, xmm12).unwrap(),
            BinKind::MinPs => a.minps(xmm10, xmm12).unwrap(),
            BinKind::MaxPd => a.maxpd(xmm10, xmm12).unwrap(),
        }
        a.vinsertf128(yd, yd, xmm10, lane).unwrap();
    }
}

fn sse_ymm_sqrt(a: &mut CodeAssembler, d: u32, s0: u32, pd: bool) {
    let (yd, y0) = (ymm_reg(d), ymm_reg(s0));
    for lane in 0..2i32 {
        a.vextractf128(xmm10, y0, lane).unwrap();
        if pd {
            a.sqrtpd(xmm10, xmm10).unwrap();
        } else {
            a.sqrtps(xmm10, xmm10).unwrap();
        }
        a.vinsertf128(yd, yd, xmm10, lane).unwrap();
    }
}

fn sse_ymm_shufps(a: &mut CodeAssembler, d: u32, s0: u32, s1: u32, imm_lo: i32, imm_hi: i32) {
    let (yd, y0, y1) = (ymm_reg(d), ymm_reg(s0), ymm_reg(s1));
    for lane in 0..2i32 {
        let imm = if lane == 0 { imm_lo } else { imm_hi };
        a.vextractf128(xmm10, y0, lane).unwrap();
        a.vextractf128(xmm12, y1, lane).unwrap();
        a.shufps(xmm10, xmm12, imm).unwrap();
        a.vinsertf128(yd, yd, xmm10, lane).unwrap();
    }
}

fn sse_ymm_shufpd(a: &mut CodeAssembler, d: u32, s0: u32, s1: u32, imm_lo: i32, imm_hi: i32) {
    let (yd, y0, y1) = (ymm_reg(d), ymm_reg(s0), ymm_reg(s1));
    for lane in 0..2i32 {
        let imm = if lane == 0 { imm_lo } else { imm_hi };
        a.vextractf128(xmm10, y0, lane).unwrap();
        a.vextractf128(xmm12, y1, lane).unwrap();
        a.shufpd(xmm10, xmm12, imm).unwrap();
        a.vinsertf128(yd, yd, xmm10, lane).unwrap();
    }
}

#[derive(Copy, Clone)]
enum UnpKind {
    LowPs,
    HighPs,
    LowPd,
    HighPd,
}

fn sse_ymm_unpck(a: &mut CodeAssembler, d: u32, s0: u32, s1: u32, kind: UnpKind) {
    let (yd, y0, y1) = (ymm_reg(d), ymm_reg(s0), ymm_reg(s1));
    for lane in 0..2i32 {
        a.vextractf128(xmm10, y0, lane).unwrap();
        a.vextractf128(xmm12, y1, lane).unwrap();
        match kind {
            UnpKind::LowPs => a.unpcklps(xmm10, xmm12).unwrap(),
            UnpKind::HighPs => a.unpckhps(xmm10, xmm12).unwrap(),
            UnpKind::LowPd => a.unpcklpd(xmm10, xmm12).unwrap(),
            UnpKind::HighPd => a.unpckhpd(xmm10, xmm12).unwrap(),
        }
        a.vinsertf128(yd, yd, xmm10, lane).unwrap();
    }
}

/// Map a ymm index (0–15) to the iced `AsmRegisterYmm` constant.
fn ymm_reg(i: u32) -> AsmRegisterYmm {
    match i {
        0 => ymm0,
        1 => ymm1,
        2 => ymm2,
        3 => ymm3,
        4 => ymm4,
        5 => ymm5,
        6 => ymm6,
        7 => ymm7,
        8 => ymm8,
        9 => ymm9,
        10 => ymm10,
        11 => ymm11,
        12 => ymm12,
        13 => ymm13,
        14 => ymm14,
        15 => ymm15,
        _ => unreachable!(),
    }
}

// --- task-256: VEX float cluster — vblendv m128 src2 (Celeste blocker) + imm8 static
// blends (blendps/pd + VEX) + dot products (dppd + vdpps/vdppd). Second source is staged
// into SCRATCH and read as a 128-bit memory operand. SSE-only forms diff against Unicorn
// (hardware oracle); VEX forms via vex_eq_sse (Unicorn's QEMU drops VEX.vvvv). ---

const CL_A: u128 = 0x4080_0000_4040_0000_4000_0000_3f80_0000; // f32 1,2,3,4
const CL_B: u128 = 0x4220_0000_41f0_0000_41a0_0000_4120_0000; // f32 10,20,30,40
                                                              // Alternating lane MSBs: qword1 / dword3 / word/byte high bits set, low clear.
const CL_MASK: u128 = 0x8000_0000_0000_0000_ffff_ffff_ffff_ffff;

/// task-256: the exact wild bytes that walled Celeste — `c4 e3 59 4a 1d e6 c7 07 00 30` =
/// `vblendvps xmm3, xmm4, [rip+0x7c7e6], xmm3` (VEX.128 variable-blend packed-single with an
/// m128 second source). Assert the raw byte encoding decodes to that instruction (proving
/// the mem/RIP form is the one we lift), then run the same operation with a normal memory
/// operand and check the hand-computed blend result + VEX.128 upper-lane zeroing.
#[test]
fn vblendvps_celeste_wild_bytes() {
    use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind};
    // Decode the faulting bytes and confirm the instruction shape.
    let wild = [0xc4u8, 0xe3, 0x59, 0x4a, 0x1d, 0xe6, 0xc7, 0x07, 0x00, 0x30];
    let mut dec = Decoder::with_ip(64, &wild, 0x1000, DecoderOptions::NONE);
    let insn = dec.decode();
    assert_eq!(
        insn.mnemonic(),
        Mnemonic::Vblendvps,
        "wild bytes = vblendvps"
    );
    assert_eq!(insn.op_count(), 4, "4-operand VEX blend");
    assert_eq!(insn.op_kind(0), OpKind::Register); // dst xmm3
    assert_eq!(insn.op_kind(1), OpKind::Register); // src1 xmm4 (vvvv)
    assert_eq!(insn.op_kind(2), OpKind::Memory); // src2 is m128 (the blocker)
    assert_eq!(insn.op_kind(3), OpKind::Register); // mask xmm3
    assert!(insn.is_ip_rel_memory_operand(), "src2 is [rip+disp32]");
    assert_eq!(insn.memory_displacement64(), 0x1000 + 10 + 0x7c7e6);

    // Run the same op with a normal memory operand (same lift path) and check the result.
    let o = Vector::asm(|a| {
        a.mov(rax, SCRATCH).unwrap();
        a.movdqu(xmmword_ptr(rax), xmm1).unwrap(); // stage src2 in memory
        a.vblendvps(xmm3, xmm0, xmmword_ptr(rax), xmm2).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        s.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444; // src1 (vvvv)
        s.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD; // src2 (memory)
                                                              // Per-dword blend control: only dwords 1 and 3 have their MSB set → take from src2.
        s.xmm[2] = 0x8000_0000_0000_0000_8000_0000_0000_0000;
        s.ymm_hi[3] = u128::MAX; // stale upper that VEX.128 must clear
    })
    .interpret();
    // dword0: msb clear → src1 0x4444_4444; dword1: msb set → src2 0xCCCC_CCCC;
    // dword2: msb clear → src1 0x2222_2222; dword3: msb set → src2 0xAAAA_AAAA.
    assert_eq!(o.cpu.xmm[3], 0xAAAA_AAAA_2222_2222_CCCC_CCCC_4444_4444);
    assert_eq!(o.cpu.ymm_hi[3], 0, "VEX.128 zeroes bits 255:128");
}

/// task-256: SSE4.1 variable blend `vblendvps/pd`/`vpblendvb` with a 128-bit MEMORY src2.
/// Validated against the SSE lowering (`vex_eq_sse`) — the SSE `blendv*` uses the implicit
/// XMM0 mask and `dst==src1`, so the equivalent copies src1→dst and the mask→xmm0 first.
#[test]
fn vblendv_memory_source_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(xmmword_ptr(rax), xmm1).unwrap(); // stage src2
                                                        // Mirror the SSE side's bookkeeping so the non-destination registers (xmm0,
                                                        // xmm7) end up identical; vex_eq_sse compares the whole xmm array.
            a.movaps(xmm7, xmm0).unwrap(); // src1 → xmm7
            a.movdqa(xmm0, xmm2).unwrap(); // mask → xmm0 (matches the SSE mask-in-XMM0)
            a.vblendvps(xmm3, xmm7, xmmword_ptr(rax), xmm2).unwrap();
            a.vblendvpd(xmm4, xmm7, xmmword_ptr(rax), xmm2).unwrap();
            a.vpblendvb(xmm5, xmm7, xmmword_ptr(rax), xmm2).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            // SSE blendv uses the implicit XMM0 as the mask and has dst==src1. Save src1
            // (xmm0) into xmm7 first, then load the mask (xmm2) into XMM0.
            a.movaps(xmm7, xmm0).unwrap(); // preserve src1
            a.movdqa(xmm0, xmm2).unwrap(); // mask → XMM0
            a.movaps(xmm3, xmm7).unwrap();
            a.blendvps(xmm3, xmmword_ptr(rax)).unwrap();
            a.movaps(xmm4, xmm7).unwrap();
            a.blendvpd(xmm4, xmmword_ptr(rax)).unwrap();
            a.movaps(xmm5, xmm7).unwrap();
            a.pblendvb(xmm5, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = CL_A;
            s.xmm[1] = CL_B;
            s.xmm[2] = CL_MASK;
        },
    );
}

/// task-256: SSE4.1 imm8 static blends `blendps`/`blendpd` (register + m128 src2), validated
/// against Unicorn (hardware oracle). `dst==src1`; imm8 bit i picks src2 lane i else keeps dst.
#[test]
fn blendi_sse_matches_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.blendps(xmm2, xmm1, 0b1010).unwrap(); // dwords 1,3 from src2
            a.blendpd(xmm3, xmm1, 0b10).unwrap(); // qword 1 from src2
            a.blendps(xmm4, xmmword_ptr(rax), 0b0101).unwrap(); // m128, dwords 0,2
            a.blendpd(xmm5, xmmword_ptr(rax), 0b01).unwrap(); // m128, qword 0
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[1] = CL_B;
            s.xmm[2] = CL_A;
            s.xmm[3] = CL_A;
            s.xmm[4] = CL_A;
            s.xmm[5] = CL_A;
        },
        &[],
    );
}

/// task-256: AVX `vblendps`/`vblendpd` (VEX 3-operand imm8 static blend, register + m128
/// src2). Validated against the SSE lowering (`vex_eq_sse`); the SSE form is `dst==src1`,
/// so the equivalent copies the merge base (op1) into dst first.
#[test]
fn vblendi_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.vblendps(xmm3, xmm0, xmm1, 0b0110).unwrap();
            a.vblendpd(xmm4, xmm0, xmm1, 0b01).unwrap();
            a.vblendps(xmm5, xmm0, xmmword_ptr(rax), 0b1001).unwrap(); // m128
            a.hlt().unwrap();
        },
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.movaps(xmm3, xmm0).unwrap();
            a.blendps(xmm3, xmm1, 0b0110).unwrap();
            a.movaps(xmm4, xmm0).unwrap();
            a.blendpd(xmm4, xmm1, 0b01).unwrap();
            a.movaps(xmm5, xmm0).unwrap();
            a.blendps(xmm5, xmmword_ptr(rax), 0b1001).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = CL_A;
            s.xmm[1] = CL_B;
        },
    );
}

/// task-256: SSE4.1 `dppd` double-precision dot product (register + m128 src2), validated
/// against Unicorn. `imm[5:4]` masks the two products; `imm[1:0]` broadcasts the sum.
#[test]
fn dppd_sse_matches_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.dppd(xmm2, xmm1, 0x31).unwrap(); // both products → qword 0
            a.dppd(xmm3, xmm1, 0x23).unwrap(); // product 1 only → qwords 0,1
            a.dppd(xmm4, xmmword_ptr(rax), 0x13).unwrap(); // m128, product 0 → qwords 0,1
            a.hlt().unwrap();
        },
        |s| {
            // f64 lanes: xmm2/3/4 = {2.0, 3.0}; xmm1 = {5.0, 7.0}.
            let f64x2 = |a: f64, b: f64| (a.to_bits() as u128) | ((b.to_bits() as u128) << 64);
            s.xmm[1] = f64x2(5.0, 7.0);
            s.xmm[2] = f64x2(2.0, 3.0);
            s.xmm[3] = f64x2(2.0, 3.0);
            s.xmm[4] = f64x2(2.0, 3.0);
        },
        &[],
    );
}

/// task-256: AVX `vdpps`/`vdppd` (VEX 3-operand dot product, register + m128 src2), validated
/// against the SSE lowering (`vex_eq_sse`); SSE `dpps`/`dppd` is `dst==src1`.
#[test]
fn vdp_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.vdpps(xmm3, xmm0, xmm1, 0x71).unwrap();
            a.vdppd(xmm4, xmm0, xmm1, 0x33).unwrap();
            a.vdpps(xmm5, xmm0, xmmword_ptr(rax), 0xF1).unwrap(); // m128
            a.vdppd(xmm6, xmm0, xmmword_ptr(rax), 0x31).unwrap(); // m128
            a.hlt().unwrap();
        },
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.movaps(xmm3, xmm0).unwrap();
            a.dpps(xmm3, xmm1, 0x71).unwrap();
            a.movaps(xmm4, xmm0).unwrap();
            a.dppd(xmm4, xmm1, 0x33).unwrap();
            a.movaps(xmm5, xmm0).unwrap();
            a.dpps(xmm5, xmmword_ptr(rax), 0xF1).unwrap();
            a.movaps(xmm6, xmm0).unwrap();
            a.dppd(xmm6, xmmword_ptr(rax), 0x31).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = CL_A;
            s.xmm[1] = CL_B;
        },
    );
}

// --- AVX upper-half (YMM) semantics — task-168.2 foundation. ---

#[test]
fn vex128_write_zeroes_ymm_upper() {
    let o = Vector::asm(|a| {
        a.vpxor(xmm0, xmm1, xmm2).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        s.xmm[1] = A;
        s.xmm[2] = B;
        s.ymm_hi[0] = 0xDEAD_BEEF; // stale upper that VEX.128 must clear
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[0], 0,
        "VEX.128 must zero bits 255:128 of the destination"
    );
}

/// task-252: VEX.128 `vmovlhps`/`vmovhlps` (3-operand) lower to a 64-bit-lane unpack.
/// Unicorn mis-decodes 3-operand VEX, so validate the VEX lowering against the equivalent
/// legacy-SSE 2-operand lowering (which the corpus validates against Unicorn).
#[test]
fn vmov_lhps_hlps_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.vmovlhps(xmm5, xmm0, xmm1).unwrap(); // [op1.lo, op2.lo]
            a.vmovhlps(xmm6, xmm0, xmm1).unwrap(); // [op2.hi, op1.hi]
        },
        |a| {
            a.movaps(xmm5, xmm0).unwrap();
            a.movlhps(xmm5, xmm1).unwrap(); // xmm5 = [xmm0.lo, xmm1.lo]
            a.movaps(xmm6, xmm0).unwrap();
            a.movhlps(xmm6, xmm1).unwrap(); // xmm6 = [xmm1.hi, xmm0.hi]
        },
        |c| {
            c.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
            c.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
        },
    );
}

/// task-252: the exact wild shape `vmovlhps %xmm0,%xmm1,%xmm0` (dst == op2). `VUnpackLow`
/// reads both sources before writing dst, so the alias is safe: result = [op1.lo, op2.lo]
/// with op2 the ORIGINAL xmm0. Hand-computed oracle; also asserts VEX.128 zeroes 255:128.
#[test]
fn vmovlhps_dst_aliases_src2() {
    let o = Vector::asm(|a| {
        a.vmovlhps(xmm0, xmm1, xmm0).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        s.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
        s.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
        s.ymm_hi[0] = 0xDEAD_BEEF; // stale upper that VEX.128 must clear
    })
    .interpret();
    // dst[63:0] = op1.lo = xmm1.lo = 0xCCCC_CCCC_DDDD_DDDD;
    // dst[127:64] = op2.lo = original xmm0.lo = 0x3333_3333_4444_4444.
    assert_eq!(o.cpu.xmm[0], 0x3333_3333_4444_4444_CCCC_CCCC_DDDD_DDDD);
    assert_eq!(o.cpu.ymm_hi[0], 0, "VEX.128 zeroes bits 255:128");
}

/// task-253: SSE3 lane-duplicating moves `movddup`/`movsldup`/`movshdup`, register and
/// memory source (movddup's is an m64), validated against Unicorn. Distinct dwords so the
/// [0,1,0,1] / [0,0,2,2] / [1,1,3,3] shuffles are observable.
#[test]
fn movdup_family_match_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm0).unwrap(); // src also in memory
            a.movddup(xmm1, xmm0).unwrap();
            a.movsldup(xmm2, xmm0).unwrap();
            a.movshdup(xmm3, xmm0).unwrap();
            a.movddup(xmm4, qword_ptr(rax)).unwrap(); // m64 form
            a.movsldup(xmm5, xmmword_ptr(rax)).unwrap(); // m128
            a.movshdup(xmm6, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = 0x3333_3333_2222_2222_1111_1111_0000_0000;
        },
        &[],
    );
}

/// task-253: VEX.128 `vmovddup`/`vmovsldup`/`vmovshdup` lower to the same shuffle as the
/// legacy forms; validate against the legacy-SSE lowering (Unicorn's AVX decode is unfit).
#[test]
fn vmovdup_family_vex_eq_sse() {
    vex_eq_sse(
        |a| {
            a.vmovddup(xmm1, xmm0).unwrap();
            a.vmovsldup(xmm2, xmm0).unwrap();
            a.vmovshdup(xmm3, xmm0).unwrap();
        },
        |a| {
            a.movddup(xmm1, xmm0).unwrap();
            a.movsldup(xmm2, xmm0).unwrap();
            a.movshdup(xmm3, xmm0).unwrap();
        },
        |c| {
            c.xmm[0] = 0x3333_3333_2222_2222_1111_1111_0000_0000;
        },
    );
}

#[test]
fn legacy_sse_preserves_ymm_upper() {
    let o = Vector::asm(|a| {
        a.pxor(xmm0, xmm1).unwrap(); // legacy SSE: leaves the upper half untouched
        a.hlt().unwrap();
    })
    .init(|s| {
        s.xmm[0] = A;
        s.xmm[1] = B;
        s.ymm_hi[0] = 0x00C0_FFEE;
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[0], 0x00C0_FFEE,
        "legacy SSE must preserve the YMM upper half"
    );
}

#[test]
fn fwait_is_a_noop() {
    // 0x9B (FWAIT/WAIT) is an x87 sync barrier; with no pending unmasked x87
    // exceptions it must not perturb any architectural state (task-194).
    diff(
        |a| {
            a.mov(rax, 0x1234_5678_9abc_def0u64).unwrap();
            a.wait().unwrap(); // 0x9B
            a.add(rax, 1i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn vzeroupper_clears_all_upper() {
    let o = Vector::asm(|a| {
        a.vzeroupper().unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        for (i, h) in s.ymm_hi.iter_mut().enumerate() {
            *h = 0x1111 * (i as u128 + 1);
        }
    })
    .interpret();
    assert!(
        o.cpu.ymm_hi.iter().all(|&h| h == 0),
        "vzeroupper must clear every YMM upper half"
    );
}

/// Packed float↔int converts (task-239): `cvtdq2ps/cvtps2dq/cvttps2dq/cvtdq2pd/cvtps2pd/
/// cvtpd2ps/cvtpd2dq/cvttpd2dq`. Inputs are all in-range (the x86 integer-indefinite
/// result on overflow/NaN is deferred, matching the scalar `cvt` path), so the saturating
/// interpreter result equals real hardware. Rounding (`cvt*` = nearest-even) vs truncation
/// (`cvtt*`) and the upper-64-zeroing of the narrowing forms are all exercised.
#[test]
fn cvt_packed_int_float_match_unicorn() {
    diff(
        |a| {
            // i32×4 [1, -2, 3, 100]
            a.mov(rax, 0xFFFF_FFFE_0000_0001u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0x0000_0064_0000_0003u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm0, xmmword_ptr(SCRATCH)).unwrap();
            a.cvtdq2ps(xmm1, xmm0).unwrap(); // → f32 [1,-2,3,100]
            a.cvtdq2pd(xmm2, xmm0).unwrap(); // low 2 → f64 [1,-2]

            // f32×4 [1.5, -2.5, 3.5, -100.75]
            a.mov(rax, 0xC020_0000_3FC0_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0xC2C9_8000_4060_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm3, xmmword_ptr(SCRATCH)).unwrap();
            a.cvtps2dq(xmm4, xmm3).unwrap(); // round-even → [2,-2,4,-101]
            a.cvttps2dq(xmm5, xmm3).unwrap(); // trunc → [1,-2,3,-100]
            a.cvtps2pd(xmm6, xmm3).unwrap(); // low 2 → f64 [1.5,-2.5]

            // f64×2 [2.5, -3.5]
            a.mov(rax, 0x4004_0000_0000_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0xC00C_0000_0000_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm7, xmmword_ptr(SCRATCH)).unwrap();
            a.cvtpd2ps(xmm8, xmm7).unwrap(); // → f32 [2.5,-3.5,0,0]
            a.cvtpd2dq(xmm9, xmm7).unwrap(); // round-even → [2,-4,0,0]
            a.cvttpd2dq(xmm10, xmm7).unwrap(); // trunc → [2,-3,0,0]

            // Memory-source form (cvtps2dq m128) exercises the VLoad path: f32 [1,2,3,4].
            a.mov(rax, 0x4000_0000_3F80_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 16), rax).unwrap();
            a.mov(rax, 0x4080_0000_4040_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 24), rax).unwrap();
            a.cvtps2dq(xmm11, xmmword_ptr(SCRATCH + 16)).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// VEX.128 packed converts (`vcvtps2dq/vcvttps2dq/vcvtdq2ps/vcvtps2pd/vcvtpd2ps/
/// vcvtpd2dq/vcvttpd2dq/vcvtdq2pd`, task-239) match their legacy-SSE equivalents on the
/// interpreter. Unicorn's QEMU mis-decodes VEX 3-operand forms so it can't be the AVX
/// oracle here; the SSE arm is already unicorn-validated above. Inputs are seeded via the
/// snapshot: xmm0 = f32 [1.5,-2.5,3.5,-100.75], xmm3 = f64 [2.5,-3.5].
#[test]
fn cvt_packed_vex128_matches_sse() {
    let seed = |s: &mut CpuSnapshot| {
        s.xmm[0] = 0xC2C9_8000_4060_0000_C020_0000_3FC0_0000u128;
        s.xmm[3] = 0xC00C_0000_0000_0000_4004_0000_0000_0000u128;
    };
    vex_eq_sse(
        |a| {
            a.vcvtps2dq(xmm1, xmm0).unwrap();
            a.vcvttps2dq(xmm2, xmm0).unwrap();
            a.vcvtdq2ps(xmm4, xmm1).unwrap();
            a.vcvtps2pd(xmm5, xmm0).unwrap();
            a.vcvtpd2ps(xmm6, xmm3).unwrap();
            a.vcvtpd2dq(xmm7, xmm3).unwrap();
            a.vcvttpd2dq(xmm8, xmm3).unwrap();
            a.vcvtdq2pd(xmm9, xmm1).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.cvtps2dq(xmm1, xmm0).unwrap();
            a.cvttps2dq(xmm2, xmm0).unwrap();
            a.cvtdq2ps(xmm4, xmm1).unwrap();
            a.cvtps2pd(xmm5, xmm0).unwrap();
            a.cvtpd2ps(xmm6, xmm3).unwrap();
            a.cvtpd2dq(xmm7, xmm3).unwrap();
            a.cvttpd2dq(xmm8, xmm3).unwrap();
            a.cvtdq2pd(xmm9, xmm1).unwrap();
            a.hlt().unwrap();
        },
        seed,
    );
}

/// Register-count packed shifts `psll/psrl/psra {w,d,q} xmm, xmm` (task-237 native path):
/// the count is the full low qword of the second operand. Covers logical L/R, arithmetic
/// R, and x86 over-shift (count ≥ lane width → 0 for logical, sign-fill for arithmetic)
/// across word/dword/qword lanes. Must match real hardware bit-for-bit.
#[test]
fn shift_reg_count_match_unicorn() {
    diff(
        |a| {
            a.mov(rax, 0x8899_AABB_1122_3344u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0xF000_0001_0000_0010u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm0, xmmword_ptr(SCRATCH)).unwrap();
            // In-range count = 3.
            a.mov(rax, 3u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.movdqa(xmm2, xmm0).unwrap();
            a.pslld(xmm2, xmm1).unwrap();
            a.movdqa(xmm3, xmm0).unwrap();
            a.psrld(xmm3, xmm1).unwrap();
            a.movdqa(xmm4, xmm0).unwrap();
            a.psrad(xmm4, xmm1).unwrap();
            a.movdqa(xmm5, xmm0).unwrap();
            a.psllw(xmm5, xmm1).unwrap();
            a.movdqa(xmm6, xmm0).unwrap();
            a.psrlq(xmm6, xmm1).unwrap();
            // Over-shift count = 40 (≥ dword/word width).
            a.mov(rax, 40u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.movdqa(xmm7, xmm0).unwrap();
            a.psrld(xmm7, xmm1).unwrap(); // logical → 0
            a.movdqa(xmm8, xmm0).unwrap();
            a.psrad(xmm8, xmm1).unwrap(); // arith → sign fill
            a.movdqa(xmm9, xmm0).unwrap();
            a.pslld(xmm9, xmm1).unwrap(); // logical left → 0
            a.movdqa(xmm10, xmm0).unwrap();
            a.psraw(xmm10, xmm1).unwrap(); // arith word over (40 ≥ 16) → sign fill
                                           // Over-shift count = 100 (≥ qword width).
            a.mov(rax, 100u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.movdqa(xmm11, xmm0).unwrap();
            a.psrlq(xmm11, xmm1).unwrap(); // logical → 0
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// Upper-bits (255:128) semantics for register-count shifts (task-237): legacy-SSE
/// `pslld xmm, xmm` PRESERVES the destination's YMM upper (SDM: non-VEX SSE never touches
/// bits above 128); VEX.128 `vpslld` CLEARS it. Unicorn can't seed/oracle the YMM upper
/// here (cf. `vex128_write_zeroes_ymm_upper`, also interp-only), so this asserts the
/// interpreter's values directly; `shift_reg_upper_bits_match_interp` (jit.rs) proves the
/// JIT lowering equals the interpreter, so the JIT inherits both behaviours.
#[test]
fn shift_reg_ymm_upper_semantics() {
    let o = Vector::asm(|a| {
        a.pslld(xmm0, xmm1).unwrap(); // SSE → preserve ymm_hi[0]
        a.vpslld(xmm3, xmm0, xmm1).unwrap(); // VEX.128 → zero ymm_hi[3]
        a.hlt().unwrap();
    })
    .init(|s| {
        s.xmm[0] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
        s.xmm[1] = 2;
        s.ymm_hi[0] = 0x0000_DEAD_BEEF_CAFE;
        s.ymm_hi[3] = 0x0000_1234_5678_9ABC;
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[0], 0x0000_DEAD_BEEF_CAFE,
        "legacy-SSE pslld must preserve bits 255:128"
    );
    assert_eq!(o.cpu.ymm_hi[3], 0, "VEX.128 vpslld must zero bits 255:128");
}

/// MOVMSKPS / MOVMSKPD (task-240): pack the packed-float sign bits into a GPR. Regression
/// for the Doom/unemups4 `movmskpd %xmm0,%esi` (66 0F 50 F0) trap. Covers all-neg, all-pos,
/// and mixed sign patterns for both the 2-double and 4-single forms; must match hardware.
#[test]
fn movmsk_ps_pd_match_unicorn() {
    diff(
        |a| {
            // xmm0 = f64 [-1.0, -1.0] (both sign bits set) → movmskpd = 0b11 = 3.
            a.mov(rax, 0xBFF0_0000_0000_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm0, xmmword_ptr(SCRATCH)).unwrap();
            a.movmskpd(esi, xmm0).unwrap(); // the exact faulting encoding

            // xmm1 = f64 [+2.0, -3.0] → lane0=0, lane1=1 → 0b10 = 2.
            a.mov(rax, 0x4000_0000_0000_0000u64).unwrap(); // +2.0
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0xC008_0000_0000_0000u64).unwrap(); // -3.0
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm1, xmmword_ptr(SCRATCH)).unwrap();
            a.movmskpd(edi, xmm1).unwrap();

            // xmm2 = f32 [-1, +2, -3, +4] → lanes 0,2 set → 0b0101 = 5.
            a.mov(rax, 0x4000_0000_BF80_0000u64).unwrap(); // -1.0, +2.0
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0x4080_0000_C040_0000u64).unwrap(); // -3.0, +4.0
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm2, xmmword_ptr(SCRATCH)).unwrap();
            a.movmskps(eax, xmm2).unwrap();

            // xmm3 = f32 all-negative → 0b1111 = 15.
            a.mov(rax, 0xBF80_0000_BF80_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm3, xmmword_ptr(SCRATCH)).unwrap();
            a.movmskps(ecx, xmm3).unwrap();

            // xmm4 = f32 all-positive → 0.
            a.mov(rax, 0x3F80_0000_3F80_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm4, xmmword_ptr(SCRATCH)).unwrap();
            a.movmskps(edx, xmm4).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

// --- SSE4.1 / AVX ROUND family (task-242). Legacy `round{ss,sd,ps,pd}` are diffed
// against Unicorn (the hardware oracle for the rounding math + all four imm8 modes);
// the VEX.128 `vround*` forms use `vex_eq_sse` (Unicorn's QEMU drops VEX.vvvv, so it
// can't decode the 3-operand scalar forms). imm8 bits[1:0] select the mode
// (0=nearest-even, 1=floor, 2=ceil, 3=trunc); bit2 = use MXCSR RC — not modelled, so
// treated as nearest-even; bit3 = suppress-precision — a no-op for us. The blocker is
// `vroundsd $0x9,%xmm1,%xmm0,%xmm1` (floor + suppress-precision). ---

// Packed-double bit patterns: [1.5, -1.5] and [2.5, -2.5] etc. (lane0 = low qword).
const RND_PD_A: u128 = 0xBFF8_0000_0000_0000_3FF8_0000_0000_0000; // [1.5, -1.5]
const RND_PD_B: u128 = 0xC004_0000_0000_0000_4004_0000_0000_0000; // [2.5, -2.5]
                                                                  // Packed-single bit patterns: [0.4, -0.4, 2.5, -2.5] (lane0 = low dword).
const RND_PS_A: u128 = 0xC020_0000_4020_0000_BECC_CCCD_3ECC_CCCD; // [0.4, -0.4, 2.5, -2.5]

fn seed_round(s: &mut CpuSnapshot) {
    s.xmm[0] = RND_PD_A;
    s.xmm[1] = RND_PD_B;
    s.xmm[2] = RND_PS_A;
    // A distinct upper qword so scalar merge (bits[127:64] from op1) is observable.
    s.xmm[3] = 0xDEAD_BEEF_CAFE_F00D_4008_0000_0000_0000; // low = 3.0
}

/// Legacy SSE4.1 `roundsd`/`roundss` (scalar): round the low element, keep the upper
/// bits of the destination. All four imm8 modes vs Unicorn, on ±half-integers (ties)
/// and ±0.4 (directed rounding differs from nearest).
#[test]
fn roundsd_roundss_scalar_all_modes() {
    for mode in 0u32..4 {
        diff(
            |a| {
                a.roundsd(xmm4, xmm0, mode).unwrap(); // round(1.5) per mode, keep xmm4[127:64]
                a.roundsd(xmm5, xmm1, mode).unwrap(); // round(2.5)
                a.roundss(xmm6, xmm2, mode).unwrap(); // round(0.4f)
                a.hlt().unwrap();
            },
            |s| {
                seed_round(s);
                s.xmm[4] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
                s.xmm[5] = 0x9999_AAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0000;
                s.xmm[6] = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;
            },
            &[],
        );
    }
}

/// Legacy SSE4.1 `roundpd`/`roundps` (packed): round every lane. All four imm8 modes
/// vs Unicorn.
#[test]
fn roundpd_roundps_packed_all_modes() {
    for mode in 0u32..4 {
        diff(
            |a| {
                a.roundpd(xmm4, xmm0, mode).unwrap(); // [1.5, -1.5]
                a.roundpd(xmm5, xmm1, mode).unwrap(); // [2.5, -2.5]
                a.roundps(xmm6, xmm2, mode).unwrap(); // [0.4, -0.4, 2.5, -2.5]
                a.hlt().unwrap();
            },
            seed_round,
            &[],
        );
    }
}

/// imm8 bit2 set = "use MXCSR RC" (ignore bits[1:0]). We don't model MXCSR RC and treat
/// this as nearest-even; the guest's default MXCSR RC is also round-to-nearest, so Unicorn
/// (which honours MXCSR) must agree. Covers imm 0x04 (bit2 alone) and 0x0C (bit2 +
/// suppress-precision) across scalar and packed — the previously-untested decode path.
#[test]
fn round_mxcsr_rc_bit_matches_unicorn() {
    for imm in [0x04u32, 0x0C] {
        diff(
            |a| {
                a.roundsd(xmm4, xmm1, imm).unwrap(); // round(2.5) per MXCSR -> nearest = 2.0
                a.roundss(xmm5, xmm2, imm).unwrap(); // round(0.4f) -> nearest = 0.0
                a.roundpd(xmm6, xmm0, imm).unwrap(); // [1.5,-1.5] -> nearest = [2.0,-2.0]
                a.roundps(xmm7, xmm2, imm).unwrap(); // [0.4,-0.4,2.5,-2.5] -> [0,0,2,-2]
                a.hlt().unwrap();
            },
            |s| {
                seed_round(s);
                s.xmm[4] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
                s.xmm[5] = 0x9999_AAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0000;
            },
            &[],
        );
    }
}

/// The exact faulting instruction from Mono: `vroundsd $0x9,%xmm1,%xmm0,%xmm1`
/// (floor + suppress-precision). The VEX scalar form keeps bits[127:64] from the first
/// source (op1 = xmm0 here), rounds op2's low double, and zeroes bits[255:128].
#[test]
fn vroundsd_blocker_floor_suppress_precision() {
    vex_eq_sse(
        |a| {
            // vroundsd xmm1, xmm0, xmm1, 0x09  -> bytes c4 e3 79 0b c9 09
            a.vroundsd(xmm1, xmm0, xmm1, 0x09u32).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            // SSE roundsd is 2-operand (dst==src1). Round op2's low in place (upper of
            // xmm1 is left untouched), then overwrite the low 64 bits with op1's upper?
            // No — VROUNDSD keeps op1's *upper* and op2's *rounded low*. So: floor op2's
            // low in place (xmm1 low = floor, xmm1 upper still = op2 upper), then splice
            // op1's upper qword over it via shufpd (lane0 from xmm1, lane1 from xmm0).
            a.roundsd(xmm1, xmm1, 0x09u32).unwrap(); // xmm1 = [floor(op2.lo), op2.hi]
            a.shufpd(xmm1, xmm0, 0b10).unwrap(); // lo=xmm1.lo, hi=xmm0.hi
            a.hlt().unwrap();
        },
        seed_round,
    );
}

/// VEX.128 scalar `vroundsd`/`vroundss` (3-operand) across all four modes: low lane from
/// round(op2), bits above from op1. Validated against the corpus-trusted SSE lowering.
#[test]
fn vex128_vroundsd_vroundss_scalar_all_modes() {
    for mode in 0u32..4 {
        vex_eq_sse(
            move |a| {
                a.vroundsd(xmm4, xmm3, xmm0, mode).unwrap(); // low=round(xmm0), hi=xmm3
                a.vroundss(xmm5, xmm3, xmm2, mode).unwrap(); // low32=round(xmm2), rest=xmm3
                a.hlt().unwrap();
            },
            move |a| {
                a.movdqa(xmm4, xmm3).unwrap();
                a.roundsd(xmm4, xmm0, mode).unwrap();
                a.movdqa(xmm5, xmm3).unwrap();
                a.roundss(xmm5, xmm2, mode).unwrap();
                a.hlt().unwrap();
            },
            seed_round,
        );
    }
}

/// VEX.128 packed `vroundpd`/`vroundps` (2-operand) across all four modes: every lane
/// rounded, bits[255:128] zeroed. Validated against SSE.
#[test]
fn vex128_vroundpd_vroundps_packed_all_modes() {
    for mode in 0u32..4 {
        vex_eq_sse(
            move |a| {
                a.vroundpd(xmm4, xmm0, mode).unwrap();
                a.vroundps(xmm5, xmm2, mode).unwrap();
                a.hlt().unwrap();
            },
            move |a| {
                a.roundpd(xmm4, xmm0, mode).unwrap();
                a.roundps(xmm5, xmm2, mode).unwrap();
                a.hlt().unwrap();
            },
            seed_round,
        );
    }
}

/// VEX.128 `vroundsd` must zero bits[255:128] of the destination even when its YMM upper
/// half was previously dirty (VEX.128 clears the upper lanes).
#[test]
fn vroundsd_zeroes_ymm_upper() {
    let o = Vector::asm(|a| {
        a.vroundsd(xmm1, xmm0, xmm0, 0x01u32).unwrap(); // floor
        a.hlt().unwrap();
    })
    .init(|s| {
        seed_round(s);
        s.ymm_hi[1] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[1], 0,
        "VEX.128 vroundsd must clear bits[255:128] of the destination"
    );
}

// --- Integer unpack / pack with a 128-bit MEMORY source (task-243). The register forms
// already lift; the gap was a memory src2 (the Mono blocker is `vpunpckldq [rip+…],xmm0,
// xmm0`). Legacy forms diffed against Unicorn (hardware oracle); VEX.128 via vex_eq_sse
// (Unicorn's QEMU drops VEX.vvvv). Second source is staged into SCRATCH and read as a
// 128-bit memory operand. ---

const UP_A: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
const UP_B: u128 = 0x1F1E_1D1C_1B1A_1918_1716_1514_1312_1110;

/// Legacy SSE2 `punpckl/h{bw,wd,dq,qdq}` and signed `packsswb/packssdw` with a 128-bit
/// memory source 2, validated against Unicorn.
#[test]
fn unpack_pack_memory_source_matches_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm2).unwrap(); // stage src2 in memory
            a.punpckldq(xmm0, xmmword_ptr(rax)).unwrap();
            a.punpckhbw(xmm1, xmmword_ptr(rax)).unwrap();
            a.punpcklqdq(xmm3, xmmword_ptr(rax)).unwrap();
            a.packsswb(xmm4, xmmword_ptr(rax)).unwrap();
            a.packssdw(xmm5, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = UP_A;
            s.xmm[1] = UP_A;
            s.xmm[2] = UP_B; // the memory operand
            s.xmm[3] = UP_A;
            s.xmm[4] = 0x0100_FF80_7F00_8000_1234_ABCD_7FFF_8001;
            s.xmm[5] = 0x0001_0002_FFFF_FFFE_7FFF_FFFF_8000_0000;
        },
        &[],
    );
}

/// The Mono blocker `vpunpckldq [mem], xmm0, xmm0` plus the other VEX.128 interleaves and
/// `vpackssdw`, all with a memory src2, validated against the SSE lowering (`vex_eq_sse`).
#[test]
fn vex128_unpack_pack_memory_source() {
    vex_eq_sse(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(xmmword_ptr(rax), xmm2).unwrap();
            a.vpunpckldq(xmm0, xmm0, xmmword_ptr(rax)).unwrap(); // the blocker shape
            a.vpunpckhwd(xmm1, xmm3, xmmword_ptr(rax)).unwrap(); // non-destructive (a != dst)
            a.vpackssdw(xmm4, xmm5, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm2).unwrap();
            a.punpckldq(xmm0, xmmword_ptr(rax)).unwrap();
            a.movdqa(xmm1, xmm3).unwrap();
            a.punpckhwd(xmm1, xmmword_ptr(rax)).unwrap();
            a.movdqa(xmm4, xmm5).unwrap();
            a.packssdw(xmm4, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = UP_A;
            s.xmm[1] = UP_A;
            s.xmm[2] = UP_B;
            s.xmm[3] = UP_B;
            s.xmm[5] = 0x0001_0002_FFFF_FFFE_7FFF_FFFF_8000_0000;
        },
    );
}

/// VEX.128 `vpunpckldq [mem]` must zero bits[255:128] of the destination even when its YMM
/// upper half was previously dirty.
#[test]
fn vpunpckldq_mem_zeroes_ymm_upper() {
    let o = Vector::asm(|a| {
        a.mov(rax, SCRATCH).unwrap();
        a.vmovdqu(xmmword_ptr(rax), xmm2).unwrap();
        a.vpunpckldq(xmm0, xmm0, xmmword_ptr(rax)).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        s.xmm[0] = UP_A;
        s.xmm[2] = UP_B;
        s.ymm_hi[0] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[0], 0,
        "VEX.128 vpunpckldq [mem] must clear bits[255:128] of the destination"
    );
}

// --- SSE3 lane-combining packed float: h{add,sub}p{s,d} / addsubp{s,d} (task-244).
// Genuinely-new ops (no prior SSE lift). Legacy 2-operand forms diffed against Unicorn
// (hardware oracle); VEX.128 3-operand forms via vex_eq_sse (Unicorn drops VEX.vvvv).
// Includes the exact Mono blocker `vhaddpd xmm0, xmm0, xmm0`. ---

// f64 lanes: xmm0=[1.5, 2.5], xmm1=[10.0, 20.0].
const HF_PD_A: u128 = 0x4004_0000_0000_0000_3FF8_0000_0000_0000;
const HF_PD_B: u128 = 0x4034_0000_0000_0000_4024_0000_0000_0000;
// f32 lanes: xmm2=[1.0, 2.0, 3.0, 4.0], xmm3=[10.0, 20.0, 30.0, 40.0].
const HF_PS_A: u128 = 0x4080_0000_4040_0000_4000_0000_3F80_0000;
const HF_PS_B: u128 = 0x4220_0000_41F0_0000_41A0_0000_4120_0000;

fn seed_hfloat(s: &mut CpuSnapshot) {
    s.xmm[0] = HF_PD_A;
    s.xmm[1] = HF_PD_B;
    s.xmm[2] = HF_PS_A;
    s.xmm[3] = HF_PS_B;
}

/// Legacy SSE3 `haddp{s,d}`, `hsubp{s,d}`, `addsubp{s,d}` (register src2) vs Unicorn.
#[test]
fn hadd_hsub_addsub_matches_unicorn() {
    diff(
        |a| {
            // dst is also src1 (2-operand), so copy a fresh operand into each dst first.
            a.movdqa(xmm4, xmm0).unwrap();
            a.haddpd(xmm4, xmm1).unwrap();
            a.movdqa(xmm5, xmm0).unwrap();
            a.hsubpd(xmm5, xmm1).unwrap();
            a.movdqa(xmm6, xmm0).unwrap();
            a.addsubpd(xmm6, xmm1).unwrap();
            a.movdqa(xmm7, xmm2).unwrap();
            a.haddps(xmm7, xmm3).unwrap();
            a.movdqa(xmm8, xmm2).unwrap();
            a.hsubps(xmm8, xmm3).unwrap();
            a.movdqa(xmm9, xmm2).unwrap();
            a.addsubps(xmm9, xmm3).unwrap();
            a.hlt().unwrap();
        },
        seed_hfloat,
        &[],
    );
}

/// Legacy forms with a 128-bit MEMORY source2 vs Unicorn.
#[test]
fn hadd_addsub_memory_source_matches_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap(); // f64 src2 in memory
            a.movdqa(xmm4, xmm0).unwrap();
            a.haddpd(xmm4, xmmword_ptr(rax)).unwrap();
            a.movdqa(xmm5, xmm0).unwrap();
            a.addsubpd(xmm5, xmmword_ptr(rax)).unwrap();

            a.movdqu(xmmword_ptr(rax), xmm3).unwrap(); // f32 src2 in memory
            a.movdqa(xmm6, xmm2).unwrap();
            a.haddps(xmm6, xmmword_ptr(rax)).unwrap();
            a.movdqa(xmm7, xmm2).unwrap();
            a.hsubps(xmm7, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        seed_hfloat,
        &[],
    );
}

/// The exact Mono blocker `vhaddpd xmm0, xmm0, xmm0` (all three operands the same) plus
/// the other VEX.128 forms, validated against the SSE lowering (`vex_eq_sse`).
#[test]
fn vex128_hadd_hsub_addsub() {
    vex_eq_sse(
        |a| {
            a.vhaddpd(xmm0, xmm0, xmm0).unwrap(); // the blocker: result = [x0+x1, x0+x1]
            a.vhsubpd(xmm4, xmm1, xmm0).unwrap(); // non-destructive, distinct operands
            a.vaddsubpd(xmm5, xmm1, xmm0).unwrap();
            a.vhaddps(xmm6, xmm3, xmm2).unwrap();
            a.vhsubps(xmm7, xmm3, xmm2).unwrap();
            a.vaddsubps(xmm8, xmm3, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.haddpd(xmm0, xmm0).unwrap();
            a.movdqa(xmm4, xmm1).unwrap();
            a.hsubpd(xmm4, xmm0).unwrap();
            a.movdqa(xmm5, xmm1).unwrap();
            a.addsubpd(xmm5, xmm0).unwrap();
            a.movdqa(xmm6, xmm3).unwrap();
            a.haddps(xmm6, xmm2).unwrap();
            a.movdqa(xmm7, xmm3).unwrap();
            a.hsubps(xmm7, xmm2).unwrap();
            a.movdqa(xmm8, xmm3).unwrap();
            a.addsubps(xmm8, xmm2).unwrap();
            a.hlt().unwrap();
        },
        seed_hfloat,
    );
}

/// VEX.128 `vaddsubpd [mem]` must zero bits[255:128] even when the YMM upper was dirty.
#[test]
fn vhfloat_mem_zeroes_ymm_upper() {
    let o = Vector::asm(|a| {
        a.mov(rax, SCRATCH).unwrap();
        a.vmovdqu(xmmword_ptr(rax), xmm1).unwrap();
        a.vaddsubpd(xmm0, xmm0, xmmword_ptr(rax)).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        seed_hfloat(s);
        s.ymm_hi[0] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[0], 0,
        "VEX.128 vaddsubpd [mem] must clear bits[255:128] of the destination"
    );
}

// --- Non-temporal moves (task-246). The cache-bypass hint is a no-op in our coherent
// model, so these lower like the aligned movdqa/movaps path. `movntdqa` is the aligned
// non-temporal *load*; the VEX forms add upper-zeroing on the load. Includes the exact
// libc blocker `vmovntdq [mem], xmm0`. ---

/// Legacy `movntdqa` (aligned non-temporal load, 66 0F38 2A): store a value to aligned
/// scratch, load it back with `movntdqa`, assert it round-trips (vs Unicorn).
#[test]
fn movntdqa_load_matches_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap(); // stage the value (aligned)
            a.movntdqa(xmm2, xmmword_ptr(rax)).unwrap(); // non-temporal load
            a.hlt().unwrap();
        },
        |c| c.xmm[1] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100,
        &[],
    );
}

/// VEX non-temporal moves: the store `vmovntdq [mem], xmm` (the exact blocker), plus
/// `vmovntps`/`vmovntpd` stores and the `vmovntdqa` load. Validated against the aligned
/// SSE equivalents (`vex_eq_sse`) — round-tripped through aligned scratch memory.
#[test]
fn vex128_movnt_moves() {
    vex_eq_sse(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovntdq(xmmword_ptr(rax), xmm1).unwrap(); // the blocker: store xmm -> [mem]
            a.vmovntps(xmmword_ptr(rax + 16), xmm2).unwrap();
            a.vmovntpd(xmmword_ptr(rax + 32), xmm3).unwrap();
            a.vmovntdqa(xmm4, xmmword_ptr(rax)).unwrap(); // non-temporal load [mem] -> xmm
            a.vmovdqu(xmm5, xmmword_ptr(rax + 16)).unwrap(); // read back the ntps store
            a.vmovdqu(xmm6, xmmword_ptr(rax + 32)).unwrap(); // read back the ntpd store
            a.hlt().unwrap();
        },
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movntdq(xmmword_ptr(rax), xmm1).unwrap();
            a.movntps(xmmword_ptr(rax + 16), xmm2).unwrap();
            a.movntpd(xmmword_ptr(rax + 32), xmm3).unwrap();
            a.movdqa(xmm4, xmmword_ptr(rax)).unwrap();
            a.movdqu(xmm5, xmmword_ptr(rax + 16)).unwrap();
            a.movdqu(xmm6, xmmword_ptr(rax + 32)).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[1] = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
            s.xmm[2] = 0xF0F1_F2F3_F4F5_F6F7_F8F9_FAFB_FCFD_FEFF;
            s.xmm[3] = 0x1122_3344_5566_7788_99AA_BBCC_DDEE_FF00;
        },
    );
}

/// VEX `vmovntdqa` (load) must zero bits[255:128] of the destination even when its YMM
/// upper half was previously dirty (VEX.128 clears the upper lanes).
#[test]
fn vmovntdqa_load_zeroes_ymm_upper() {
    let o = Vector::asm(|a| {
        a.mov(rax, SCRATCH).unwrap();
        a.vmovdqu(xmmword_ptr(rax), xmm1).unwrap();
        a.vmovntdqa(xmm0, xmmword_ptr(rax)).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        s.xmm[1] = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
        s.ymm_hi[0] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[0], 0,
        "VEX.128 vmovntdqa must clear bits[255:128] of the destination"
    );
}

// --- SSSE3 packed-integer horizontal add/sub: ph{add,sub}{w,d,sw} (task-247).
// Genuinely-new ops. Legacy 2-operand forms diffed against Unicorn (hardware oracle);
// VEX.128 3-operand forms via vex_eq_sse. Includes the exact Mono blocker
// `vphaddd xmm0, xmm0, xmm0`. The `sw` variants signed-saturate 16-bit results. ---

// Word lanes chosen to exercise saturation: 0x7FFF+0x7FFF (overflow +), 0x8000+0x8000
// (overflow -), 0x0001+0xFFFF (=0), 0x4000+0x4000 (=0x8000 -> saturates +).
const HI_A: u128 = 0x4000_4000_FFFF_0001_8000_8000_7FFF_7FFF;
const HI_B: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
// Dword lanes for phaddd/phsubd (wrap semantics; include a big pair that wraps i32).
const HI_D_A: u128 = 0x7FFF_FFFF_7FFF_FFFF_0000_0002_0000_0003;
const HI_D_B: u128 = 0x8000_0000_8000_0000_FFFF_FFFF_0000_0005;

fn seed_hint(s: &mut CpuSnapshot) {
    s.xmm[0] = HI_A;
    s.xmm[1] = HI_B;
    s.xmm[2] = HI_D_A;
    s.xmm[3] = HI_D_B;
}

/// Legacy SSSE3 `phaddw/phaddd/phaddsw`, `phsubw/phsubd/phsubsw` (register src2) vs Unicorn.
#[test]
fn phadd_phsub_matches_unicorn() {
    diff(
        |a| {
            // dst is also src1 (2-operand), so copy a fresh operand into each dst first.
            a.movdqa(xmm4, xmm0).unwrap();
            a.phaddw(xmm4, xmm1).unwrap();
            a.movdqa(xmm5, xmm0).unwrap();
            a.phsubw(xmm5, xmm1).unwrap();
            a.movdqa(xmm6, xmm0).unwrap();
            a.phaddsw(xmm6, xmm1).unwrap(); // signed-saturating
            a.movdqa(xmm7, xmm0).unwrap();
            a.phsubsw(xmm7, xmm1).unwrap(); // signed-saturating
            a.movdqa(xmm8, xmm2).unwrap();
            a.phaddd(xmm8, xmm3).unwrap();
            a.movdqa(xmm9, xmm2).unwrap();
            a.phsubd(xmm9, xmm3).unwrap();
            a.hlt().unwrap();
        },
        seed_hint,
        &[],
    );
}

/// Legacy forms with a 128-bit MEMORY source2 vs Unicorn.
#[test]
fn phadd_phsub_memory_source_matches_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap(); // word src2 in memory
            a.movdqa(xmm4, xmm0).unwrap();
            a.phaddw(xmm4, xmmword_ptr(rax)).unwrap();
            a.movdqa(xmm5, xmm0).unwrap();
            a.phaddsw(xmm5, xmmword_ptr(rax)).unwrap();

            a.movdqu(xmmword_ptr(rax), xmm3).unwrap(); // dword src2 in memory
            a.movdqa(xmm6, xmm2).unwrap();
            a.phaddd(xmm6, xmmword_ptr(rax)).unwrap();
            a.movdqa(xmm7, xmm2).unwrap();
            a.phsubd(xmm7, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        seed_hint,
        &[],
    );
}

/// The exact Mono blocker `vphaddd xmm0, xmm0, xmm0` (all three operands the same) plus
/// the other VEX.128 forms, validated against the SSE lowering (`vex_eq_sse`).
#[test]
fn vex128_phadd_phsub() {
    vex_eq_sse(
        |a| {
            a.vphaddd(xmm0, xmm0, xmm0).unwrap(); // the blocker
            a.vphaddw(xmm4, xmm1, xmm0).unwrap(); // non-destructive, distinct operands
            a.vphsubw(xmm5, xmm1, xmm0).unwrap();
            a.vphaddsw(xmm6, xmm1, xmm0).unwrap();
            a.vphsubsw(xmm7, xmm1, xmm0).unwrap();
            a.vphaddd(xmm8, xmm3, xmm2).unwrap();
            a.vphsubd(xmm9, xmm3, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |a| {
            a.phaddd(xmm0, xmm0).unwrap();
            a.movdqa(xmm4, xmm1).unwrap();
            a.phaddw(xmm4, xmm0).unwrap();
            a.movdqa(xmm5, xmm1).unwrap();
            a.phsubw(xmm5, xmm0).unwrap();
            a.movdqa(xmm6, xmm1).unwrap();
            a.phaddsw(xmm6, xmm0).unwrap();
            a.movdqa(xmm7, xmm1).unwrap();
            a.phsubsw(xmm7, xmm0).unwrap();
            a.movdqa(xmm8, xmm3).unwrap();
            a.phaddd(xmm8, xmm2).unwrap();
            a.movdqa(xmm9, xmm3).unwrap();
            a.phsubd(xmm9, xmm2).unwrap();
            a.hlt().unwrap();
        },
        seed_hint,
    );
}

/// VEX.128 `vphaddd [mem]` must zero bits[255:128] even when the YMM upper was dirty.
#[test]
fn vphaddd_mem_zeroes_ymm_upper() {
    let o = Vector::asm(|a| {
        a.mov(rax, SCRATCH).unwrap();
        a.vmovdqu(xmmword_ptr(rax), xmm3).unwrap();
        a.vphaddd(xmm0, xmm2, xmmword_ptr(rax)).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        seed_hint(s);
        s.ymm_hi[0] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[0], 0,
        "VEX.128 vphaddd [mem] must clear bits[255:128] of the destination"
    );
}

// --- SSE2 / VEX.128 packed sum-of-absolute-differences of bytes: psadbw / vpsadbw
// (task-249). For each 64-bit half: sum(|a.byte[i] - b.byte[i]|) over the 8 unsigned
// bytes → 16-bit result in the low word of that half (bits 63:16 zeroed); the VEX.128
// form additionally clears bits 255:128. Genuinely-new ops. Legacy form diffed against
// Unicorn (hardware oracle); VEX.128 via vex_eq_sse. Edge cases: max byte diff 0x00 vs
// 0xFF → SAD 8*255 = 2040 (0x07F8); identical operands → 0; mixed values. ---

// Low half: 0x00 vs 0xFF (max diff each byte → SAD 2040). High half: 0xFF vs 0x00 (also
// 2040). So SAD(SAD_A, SAD_B) == 0x0000_0000_0000_07F8_0000_0000_0000_07F8.
const SAD_A: u128 = 0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000;
const SAD_B: u128 = 0x0000_0000_0000_0000_FFFF_FFFF_FFFF_FFFF;
// Mixed bytes for a non-degenerate diff (crosses the 64-bit half boundary differently).
const SAD_C: u128 = 0x1020_3040_5060_7080_00FF_017F_8081_02FE;
const SAD_D: u128 = 0x8070_6050_4030_2010_FF00_7F01_0180_FE02;

fn seed_sad(s: &mut CpuSnapshot) {
    s.xmm[0] = SAD_A;
    s.xmm[1] = SAD_B;
    s.xmm[2] = SAD_C;
    s.xmm[3] = SAD_D;
}

/// Legacy SSE2 `psadbw` (register + edge-case operands) vs Unicorn.
#[test]
fn psadbw_matches_unicorn() {
    diff(
        |a| {
            // dst is also src1 (2-operand), so copy a fresh operand into each dst first.
            a.movdqa(xmm4, xmm0).unwrap();
            a.psadbw(xmm4, xmm1).unwrap(); // max diff → 2040 in each half
            a.movdqa(xmm5, xmm2).unwrap();
            a.psadbw(xmm5, xmm3).unwrap(); // mixed
            a.movdqa(xmm6, xmm2).unwrap();
            a.psadbw(xmm6, xmm2).unwrap(); // identical operands → 0
            a.hlt().unwrap();
        },
        seed_sad,
        &[],
    );
}

/// Legacy `psadbw` with a 128-bit MEMORY source2 vs Unicorn.
#[test]
fn psadbw_memory_source_matches_unicorn() {
    diff(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.movdqa(xmm4, xmm0).unwrap();
            a.psadbw(xmm4, xmmword_ptr(rax)).unwrap(); // max diff via memory
            a.movdqu(xmmword_ptr(rax), xmm3).unwrap();
            a.movdqa(xmm5, xmm2).unwrap();
            a.psadbw(xmm5, xmmword_ptr(rax)).unwrap(); // mixed via memory
            a.hlt().unwrap();
        },
        seed_sad,
        &[],
    );
}

/// VEX.128 `vpsadbw` (register src2), incl. the `dst == src1` shape `vpsadbw xmm4, xmm4,
/// xmm0` (bytes c5 d9 f6 e0), validated against the SSE lowering (`vex_eq_sse`).
#[test]
fn vex128_psadbw() {
    vex_eq_sse(
        |a| {
            a.vpsadbw(xmm4, xmm0, xmm1).unwrap(); // max diff, non-destructive distinct ops
            a.vpsadbw(xmm5, xmm2, xmm3).unwrap(); // mixed
            a.vpsadbw(xmm6, xmm2, xmm2).unwrap(); // identical → 0
            a.hlt().unwrap();
        },
        |a| {
            a.movdqa(xmm4, xmm0).unwrap();
            a.psadbw(xmm4, xmm1).unwrap();
            a.movdqa(xmm5, xmm2).unwrap();
            a.psadbw(xmm5, xmm3).unwrap();
            a.movdqa(xmm6, xmm2).unwrap();
            a.psadbw(xmm6, xmm2).unwrap();
            a.hlt().unwrap();
        },
        seed_sad,
    );
}

/// The `dst == src1` instance `vpsadbw xmm4, xmm4, xmm0` (bytes c5 d9 f6 e0) — checks the
/// result and that VEX.128 zeroes bits[255:128] even when the YMM upper was dirty.
#[test]
fn vpsadbw_dst_eq_src1_and_ymm_upper() {
    let o = Vector::asm(|a| {
        a.vpsadbw(xmm4, xmm4, xmm0).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        // xmm4 all 0xFF, xmm0 all 0x00 → SAD 2040 (0x07F8) in each 64-bit half.
        s.xmm[4] = 0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF;
        s.xmm[0] = 0;
        s.ymm_hi[4] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;
    })
    .interpret();
    assert_eq!(
        o.cpu.xmm[4], 0x0000_0000_0000_07F8_0000_0000_0000_07F8,
        "vpsadbw: SAD(0xFF*8, 0x00*8) = 2040 (0x07F8) in the low word of each 64-bit half"
    );
    assert_eq!(
        o.cpu.ymm_hi[4], 0,
        "VEX.128 vpsadbw must clear bits[255:128] of the destination"
    );
}

/// VEX.128 `vpsadbw [mem]` must zero bits[255:128] even when the YMM upper was dirty.
#[test]
fn vpsadbw_mem_zeroes_ymm_upper() {
    let o = Vector::asm(|a| {
        a.mov(rax, SCRATCH).unwrap();
        a.vmovdqu(xmmword_ptr(rax), xmm1).unwrap();
        a.vpsadbw(xmm0, xmm2, xmmword_ptr(rax)).unwrap();
        a.hlt().unwrap();
    })
    .init(|s| {
        seed_sad(s);
        s.ymm_hi[0] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;
    })
    .interpret();
    assert_eq!(
        o.cpu.ymm_hi[0], 0,
        "VEX.128 vpsadbw [mem] must clear bits[255:128] of the destination"
    );
}
