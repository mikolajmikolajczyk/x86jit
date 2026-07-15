//! Coverage ratchet (task-187): the compat map tracks *presence* (does an op lift),
//! but nothing forces a newly-lifted op to have a *correctness* test. This ratchet
//! closes that gap. It asserts:
//!
//! ```text
//! lifted − fuzzer_covered − allowlist == ∅
//! ```
//!
//! i.e. every mnemonic the lifter handles is either exercised by the differential
//! fuzzer menu (`x86jit-tests/src/fuzz.rs`, oracled against Unicorn/native) or
//! explicitly waived in [`ALLOWLIST`] below (covered by a hand-written differential/
//! jit snippet, or intentionally trivial). A brand-new lift that is neither fuzzed
//! nor listed fails this test — the author must add real coverage.
//!
//! The "lifted" set comes from the same probe the compat map uses
//! (`x86jit_tests::compat::lifted_mnemonics`) — it's pure lift, no Unicorn needed —
//! so this test runs unconditionally (no `unicorn` feature gate).

use std::collections::BTreeSet;

use x86jit_tests::compat::lifted_mnemonics;

/// Mnemonics the differential fuzzer menu (`fuzz.rs`) actually emits, as iced
/// `Mnemonic` debug names (`Add`, `Paddb`, …). This is the union of what
/// `gen`/`gen32` assemble across their `emit` arms — hard-coded here because the menu
/// is small and stable, and pinning it makes the ratchet independent of the RNG.
///
/// MUST TRACK `fuzz.rs`: when you add a `FuzzInsn` generator (or a new `emit` arm),
/// add its mnemonic here so the ratchet credits it as fuzzer-covered (and drop the
/// matching [`ALLOWLIST`] entry — that's how coverage ratchets *up*).
const FUZZER_COVERED: &[&str] = &[
    // BinReg / BinImm: add/sub/adc/sbb/and/or/xor/cmp/test.
    "Add",
    "Sub",
    "Adc",
    "Sbb",
    "And",
    "Or",
    "Xor",
    "Cmp",
    "Test",
    // UnReg: inc/dec/neg/not.
    "Inc",
    "Dec",
    "Neg",
    "Not",
    // Mov family (MovImm/MovReg/Load/Store) + Movzx/Movsx.
    "Mov",
    "Movzx",
    "Movsx",
    // Setcc (all 16 condition codes).
    "Sete",
    "Setne",
    "Setb",
    "Setae",
    "Setbe",
    "Seta",
    "Setl",
    "Setge",
    "Setle",
    "Setg",
    "Sets",
    "Setns",
    "Seto",
    "Setno",
    "Setp",
    "Setnp",
    // Cmovcc (all 16 condition codes).
    "Cmove",
    "Cmovne",
    "Cmovb",
    "Cmovae",
    "Cmovbe",
    "Cmova",
    "Cmovl",
    "Cmovge",
    "Cmovle",
    "Cmovg",
    "Cmovs",
    "Cmovns",
    "Cmovo",
    "Cmovno",
    "Cmovp",
    "Cmovnp",
    // Shift/rotate: shl/shr/sar/rol/ror/rcl/rcr + double-shift shld/shrd.
    "Shl",
    "Shr",
    "Sar",
    "Rol",
    "Ror",
    "Rcl",
    "Rcr",
    "Shld",
    "Shrd",
    // Multiply/divide-ish: mul/imul (1-op + 2/3-op) + mulx.
    "Mul",
    "Imul",
    "Mulx",
    // Bit ops: bt/bts/btr/btc + tzcnt/lzcnt + popcnt + bswap.
    "Bt",
    "Bts",
    "Btr",
    "Btc",
    "Tzcnt",
    "Lzcnt",
    "Popcnt",
    "Bswap",
    // BMI1: andn/blsi/blsr/blsmsk + BMI2 shifts shlx/shrx/sarx/rorx.
    "Andn",
    "Blsi",
    "Blsr",
    "Blsmsk",
    "Shlx",
    "Shrx",
    "Sarx",
    "Rorx",
    // SSE2 packed-integer (VBin): padd*/psub*/pand/por/pxor/pandn/pcmp*/punpck*/
    // packuswb/pminub/pmaxub.
    "Paddb",
    "Paddw",
    "Paddd",
    "Paddq",
    "Psubb",
    "Psubw",
    "Psubd",
    "Psubq",
    "Pand",
    "Por",
    "Pxor",
    "Pandn",
    "Pcmpeqb",
    "Pcmpeqw",
    "Pcmpeqd",
    "Pcmpgtb",
    "Pcmpgtw",
    "Pcmpgtd",
    "Punpcklbw",
    "Punpcklwd",
    "Punpckldq",
    "Punpcklqdq",
    "Punpckhbw",
    "Punpckhwd",
    "Punpckhdq",
    "Punpckhqdq",
    "Packuswb",
    "Pminub",
    "Pmaxub",
    // SSE2 saturating add/sub, rounding average, signed packs, pmaddwd (task-190, VBin).
    "Paddsb",
    "Paddsw",
    "Paddusb",
    "Paddusw",
    "Psubsb",
    "Psubsw",
    "Psubusb",
    "Psubusw",
    "Pavgb",
    "Pavgw",
    "Packsswb",
    "Packssdw",
    "Pmaddwd",
    // SSE2 packed shifts by imm (VShiftImm): psll/psrl/psra {w,d,q}.
    "Psllw",
    "Pslld",
    "Psllq",
    "Psrlw",
    "Psrld",
    "Psrlq",
    "Psraw",
    "Psrad",
    // Shuffle/mask: pshufd + pmovmskb.
    "Pshufd",
    "Pmovmskb",
];

/// Mnemonics that are LIFTED but lack a fuzzer-menu entry. Each entry is covered by a
/// hand-written differential/jit snippet, or is intentionally trivial (nops, fences,
/// `endbr`, `ud2`, pseudo-`Db`/`Dd`/… data directives, …).
///
/// Adding a fuzzer generator for one of these and removing it here is the way to
/// ratchet coverage *up*. A NEW lifted op that is neither fuzzed nor listed here fails
/// this test — add real coverage or, as a last resort, an explicit entry with a
/// reason. Seeded (task-187) with exactly the current `lifted − fuzzer_covered` set,
/// so the test passes today.
const ALLOWLIST: &[&str] = &[
    "Addpd",
    "Addps",
    "Addsd",
    "Addss",
    // task-244: SSE3 lane-combining packed float — hand-written differential
    // (hadd_hsub_addsub_matches_unicorn / vex128_hadd_hsub_addsub / *_mem_*).
    "Addsubpd",
    "Addsubps",
    "Andnpd",
    "Andnps",
    "Andpd",
    "Andps",
    "Bextr",
    "Blendvpd",
    "Blendvps",
    "Bsf",
    "Bsr",
    "Bzhi",
    "Call",
    "Cbw",
    "Cdq",
    "Cld",
    "Cmppd",
    "Cmpps",
    "Cmpsd",
    "Cmpss",
    "Comisd",
    "Comiss",
    "Cpuid",
    "Crc32",
    // task-239: packed float↔int converts — hand-written differential
    // cvt_packed_int_float_match_unicorn (interp vs CPU, in-range) + jit
    // cvt_packed_match_interp (jit == interp on NaN/±inf/overflow, where the
    // saturating result is deferred vs x86 integer-indefinite, like scalar cvt).
    "Cvtdq2pd",
    "Cvtdq2ps",
    "Cvtpd2dq",
    "Cvtpd2ps",
    "Cvtps2dq",
    "Cvtps2pd",
    "Cvtsd2si",
    "Cvtsd2ss",
    "Cvtsi2sd",
    "Cvtsi2ss",
    "Cvtss2sd",
    "Cvtss2si",
    "Cvttpd2dq", // task-239 (packed truncating convert)
    "Cvttps2dq", // task-239
    "Cvttsd2si",
    "Cvttss2si",
    "Cwd",
    "Cwde",
    "Db",
    "Dd",
    // task-195: SSE4.1 single-precision dot product — jit test sse41_dpps_match_interp (jit
    // == interp via shared dpps helper) + native_dpps_matches_interp (bit-exact vs CPU, NaN).
    "Dpps",
    "Div",
    "Divpd",
    "Divps",
    "Divsd",
    "Divss",
    "Dq",
    "Dw",
    "Emms",  // task-208 (MMX↔x87 bridge; emms is a no-op in our model)
    "F2xm1", // task-206
    "Fabs",
    "Fadd",
    "Faddp",
    "Fchs",
    "Fcos", // task-206
    "Fcomi",
    "Fcomip",
    "Fdiv",
    "Fdivp",
    "Fdivr",
    "Fdivrp",
    "Fld",
    "Fld1",
    "Fldz",
    "Fmul",
    "Fmulp",
    "Fnstsw",
    "Fpatan", // task-206
    "Fprem",
    "Fptan",   // task-206
    "Fsin",    // task-206
    "Fsincos", // task-206
    "Fst",
    "Fstp",
    "Fstsw",
    "Fsub",
    "Fsubp",
    "Fsubr",
    "Fsubrp",
    "Fucomi",
    "Fucomip",
    "Fxch",
    "Fyl2x",   // task-206
    "Fyl2xp1", // task-206
    // task-244: SSE3 horizontal add/sub — hand-written differential.
    "Haddpd",
    "Haddps",
    "Hlt",
    "Hsubpd",
    "Hsubps",
    "Idiv",
    "In",
    "Int",
    "Int1",
    "Int3",
    // task-195: SSE4.1 lane insert + zero mask — jit test sse41_insertps_match_interp (jit ==
    // interp, inline codegen shuffle) + native_insertps_matches_interp (bit-exact vs CPU).
    "Insertps",
    "Jmp",
    "Leave",
    "Lfence",
    "Maxpd",
    "Maxps",
    "Maxsd",
    "Maxss",
    "Mfence",
    "Minpd",
    "Minps",
    "Minsd",
    "Minss",
    "Movapd",
    "Movaps",
    "Movd",
    "Movdq2q", // task-208
    "Movdqa",
    "Movdqu",
    "Movhlps",
    "Movlhps",
    // task-240: packed-float sign-mask extract — differential movmsk_ps_pd_match_unicorn
    // (interp vs CPU: all-neg/all-pos/mixed) + jit movmsk_ps_pd_match_interp.
    "Movmskpd",
    "Movmskps",
    "Movq",
    "Movq2dq", // task-208
    "Movsd",
    "Movss",
    "Movupd",
    "Movups",
    "Mulpd",
    "Mulps",
    "Mulsd",
    "Mulss",
    "Nop",
    "Orpd",
    "Orps",
    "Out",
    "Palignr",
    "Pause",
    "Pblendvb",
    "Pcmpeqq",
    "Pcmpestri",
    // task-195: SSE4.2 string compare → mask in XMM0 — jit test sse42_pcmpstrm_match_interp
    // (jit == interp via shared helper) + native_pcmpistrm_matches_interp (bit-exact vs CPU).
    "Pcmpestrm",
    "Pcmpistrm",
    "Vpcmpestrm",
    "Vpcmpistrm",
    "Pcmpgtq",
    "Pcmpistri",
    "Pdep",
    "Pext",
    "Pextrb",
    "Pextrd",
    "Pextrq",
    "Pextrw",
    // task-247: SSSE3 packed-integer horizontal add/sub — hand-written differential
    // (phadd_phsub_matches_unicorn / phadd_phsub_memory_source_* / vex128_phadd_phsub).
    "Phaddd",
    "Phaddsw",
    "Phaddw",
    "Phsubd",
    "Phsubsw",
    "Phsubw",
    "Pinsrb",
    "Pinsrd",
    "Pinsrq",
    "Pinsrw",
    "Pmaxsd",
    "Pmaxsw",
    "Pmaxud",
    "Pminsd",
    "Pminsw",
    "Pminud",
    "Pmovsxbd",
    "Pmovsxbq",
    "Pmovsxbw",
    "Pmovsxdq",
    "Pmovsxwd",
    "Pmovsxwq",
    "Pmovzxbd",
    "Pmovzxbq",
    "Pmovzxbw",
    "Pmovzxdq",
    "Pmovzxwd",
    "Pmovzxwq",
    "Pmulld",
    "Pmuludq", // task-215: native+jit tests (native_vpmuludq/vpmuludq_match_interp)
    // task-215 (caddy HTTPS): SSE word blend — jit test pblendw_match_interp.
    "Pblendw",
    // task-215 (TLS): packed multiplies — jit test packed_muls_match_interp.
    "Pmuldq",
    "Pmulhuw",
    "Pmulhw",
    "Pmullw",
    "Pop",
    "Pshufb",
    "Pshufhw",
    "Pshuflw",
    // task-210: SSSE3 psign — pure element-wise codegen, covered by the dedicated
    // `native_psign_matches_interp` (bit-exact vs real CPU) + `psign_all_variants_match_interp`.
    "Psignb",
    "Psignd",
    "Psignw",
    "Pslldq",
    "Psrldq",
    "Ptest",
    "Push",
    "Ret",
    "Roundpd",
    "Roundps",
    "Roundsd",
    "Roundss",
    // task-223: SAL is the /6 encoding alias of SHL (identical semantics, same lift
    // path). Covered by `sal_alias_matches_interp` in jit.rs; the fuzzer menu only
    // emits the /4 SHL form, so credit SAL here rather than in FUZZER_COVERED.
    "Sal",
    "Sfence",
    "Shufpd",
    "Shufps",
    "Sqrtpd",
    "Sqrtps",
    "Sqrtsd",
    "Sqrtss",
    "Std",
    "Subpd",
    "Subps",
    "Subsd",
    "Subss",
    "Syscall",
    "Ucomisd",
    "Ucomiss",
    "Ud2",
    "Vaddpd",
    "Vaddps",
    "Vaddsd",
    "Vaddss",
    // task-244: VEX.128 addsub — hand-written differential (vex_eq_sse).
    "Vaddsubpd",
    "Vaddsubps",
    "Valignd",
    "Valignq",
    "Vandnpd",
    "Vandnps",
    "Vandpd",
    "Vandps",
    // task-214: broadcast family — covered by native_broadcast_lane_matches_interp +
    // broadcast_lane_variants_match_interp (lane forms) and the scalar-broadcast lift.
    "Vbroadcastf32x2",
    "Vbroadcasti32x2",
    "Vbroadcastsd",
    "Vbroadcastss",
    "Vcomisd",
    "Vcomiss",
    // task-239: VEX.128 packed converts — cvt_packed_vex128_matches_sse (VEX == the
    // unicorn-validated SSE lowering; QEMU mis-decodes VEX so it can't be the AVX oracle).
    "Vcvtdq2pd",
    "Vcvtdq2ps",
    "Vcvtpd2dq",
    "Vcvtpd2ps",
    "Vcvtps2dq",
    "Vcvtps2pd",
    "Vcvtsd2si",
    "Vcvtsd2ss",
    "Vcvtsd2usi",
    "Vcvtsi2sd",
    "Vcvtsi2ss",
    "Vcvtss2sd",
    "Vcvtss2si",
    "Vcvtss2usi",
    "Vcvttpd2dq", // task-239
    "Vcvttps2dq", // task-239
    "Vcvttsd2si",
    "Vcvttsd2usi",
    "Vcvttss2si",
    "Vcvttss2usi",
    "Vcvtusi2sd",
    "Vcvtusi2ss",
    "Vdivpd",
    "Vdivps",
    "Vdivsd",
    "Vdivss",
    "Vextractf128",
    "Vextractf32x4",
    "Vextractf64x2",
    "Vextracti128",
    "Vextracti32x4",
    "Vextracti64x2",
    // task-168.6: `vextractps r/m32, xmm, imm8` — hand-written differential
    // (vextractps_{reg,mem}_dst_all_lanes_match_unicorn, interp vs CPU across all
    // four lanes + both dst forms) + jit (vextractps_match_interp, jit == interp).
    "Vextractps",
    "Vfmadd132pd",
    "Vfmadd132ps",
    "Vfmadd132sd",
    "Vfmadd132ss",
    "Vfmadd213pd",
    "Vfmadd213ps",
    "Vfmadd213sd",
    "Vfmadd213ss",
    "Vfmadd231pd",
    "Vfmadd231ps",
    "Vfmadd231sd",
    "Vfmadd231ss",
    "Vfmsub132pd",
    "Vfmsub132ps",
    "Vfmsub132sd",
    "Vfmsub132ss",
    "Vfmsub213pd",
    "Vfmsub213ps",
    "Vfmsub213sd",
    "Vfmsub213ss",
    "Vfmsub231pd",
    "Vfmsub231ps",
    "Vfmsub231sd",
    "Vfmsub231ss",
    "Vfnmadd132pd",
    "Vfnmadd132ps",
    "Vfnmadd132sd",
    "Vfnmadd132ss",
    "Vfnmadd213pd",
    "Vfnmadd213ps",
    "Vfnmadd213sd",
    "Vfnmadd213ss",
    "Vfnmadd231pd",
    "Vfnmadd231ps",
    "Vfnmadd231sd",
    "Vfnmadd231ss",
    "Vfnmsub132pd",
    "Vfnmsub132ps",
    "Vfnmsub132sd",
    "Vfnmsub132ss",
    "Vfnmsub213pd",
    "Vfnmsub213ps",
    "Vfnmsub213sd",
    "Vfnmsub213ss",
    "Vfnmsub231pd",
    "Vfnmsub231ps",
    "Vfnmsub231sd",
    "Vfnmsub231ss",
    // task-244: VEX.128 horizontal add/sub — hand-written differential (vex_eq_sse).
    "Vhaddpd",
    "Vhaddps",
    "Vhsubpd",
    "Vhsubps",
    "Vinsertf128",
    "Vinsertf32x4",
    "Vinsertf64x2",
    "Vinserti128",
    "Vinserti32x4",
    "Vinserti64x2",
    "Vmaxpd",
    "Vmaxps",
    "Vmaxsd",
    "Vmaxss",
    "Vminpd",
    "Vminps",
    "Vminsd",
    "Vminss",
    "Vmovapd",
    "Vmovaps",
    "Vmovd",
    "Vmovdqa",
    "Vmovdqa32",
    "Vmovdqa64",
    "Vmovdqu",
    "Vmovdqu16",
    "Vmovdqu32",
    "Vmovdqu64",
    "Vmovdqu8",
    "Vmovmskpd", // task-240 (VEX.128 sign-mask; shares the movmsk lowering)
    "Vmovmskps", // task-240
    "Vmovq",
    "Vmovsd",
    "Vmovss",
    "Vmovupd",
    "Vmovups",
    "Vmulpd",
    "Vmulps",
    "Vmulsd",
    "Vmulss",
    "Vorpd",
    "Vorps",
    "Vpabsb",
    "Vpabsd",
    "Vpabsq",
    "Vpabsw",
    "Vpackssdw",
    "Vpacksswb",
    "Vpackusdw",
    "Vpackuswb",
    "Vpaddb",
    "Vpaddd",
    "Vpaddq",
    "Vpaddw",
    "Vpalignr",
    "Vpand",
    "Vpandd",
    "Vpandn",
    "Vpandnd",
    "Vpandnq",
    "Vpandq",
    // AVX-512 masked EVEX ops (task-209): jit==interp + native bit-exact in native.rs/jit.rs.
    "Vpblendmd",
    "Vpblendmq",
    "Vpblendd", // task-215: native+jit tests (native_vpblendd/vpblendd_match_interp)
    "Vpblendw",
    "Vpbroadcastb",
    "Vpbroadcastd",
    "Vpbroadcastq",
    "Vpbroadcastw",
    "Vpcmpeqb",
    "Vpcmpeqd",
    "Vpcmpeqw",
    "Vpcmpestri",
    "Vpcmpgtb",
    "Vpcmpgtd",
    "Vpcmpgtw",
    "Vpcmpistri",
    "Vpconflictd", // task-209
    "Vpconflictq", // task-209
    "Vperm2f128",
    "Vperm2i128",
    "Vpermd",
    "Vpermi2d",
    "Vpermi2q",
    "Vpermi2w",
    "Vpermpd", // task-215: imm8 4-qword permute (jit test vpermq_mem_imm_match_interp)
    "Vpermq",
    "Vpermt2d",
    "Vpermt2q",
    "Vpermt2w",
    "Vpextrb",
    "Vpextrd",
    "Vpextrq",
    "Vpextrw",
    // task-247: VEX.128 packed-integer horizontal add/sub — hand-written differential
    // (vex128_phadd_phsub via vex_eq_sse; incl the blocker vphaddd xmm0,xmm0,xmm0).
    "Vphaddd",
    "Vphaddsw",
    "Vphaddw",
    "Vphsubd",
    "Vphsubsw",
    "Vphsubw",
    "Vpinsrb",
    "Vpinsrd",
    "Vpinsrq",
    "Vpinsrw",
    "Vpmaxsd",
    "Vpmaxsq",
    "Vpmaxub",
    "Vpmaxud",
    "Vpmaxuq",
    "Vpminsd",
    "Vpminsq",
    "Vpminub",
    "Vpminud",
    "Vpminuq",
    "Vpmovdb",
    "Vpmovdw",
    "Vplzcntd", // task-209
    "Vplzcntq", // task-209
    "Vpmovmskb",
    "Vpmovqb",
    "Vpmovqd",
    "Vpmovqw",
    "Vpmovsxbd",
    "Vpmovsxbq",
    "Vpmovsxbw",
    "Vpmovsxdq",
    "Vpmovsxwd",
    "Vpmovsxwq",
    "Vpmovwb",
    "Vpmovzxbd",
    "Vpmovzxbq",
    "Vpmovzxbw",
    "Vpmovzxdq",
    "Vpmovzxwd",
    "Vpmovzxwq",
    "Vpmullq",
    "Vpmuludq", // task-215: native+jit tests (native_vpmuludq/vpmuludq_match_interp)
    // task-215 (TLS): packed multiplies — jit test packed_muls_match_interp.
    "Vpmuldq",
    "Vpmulhuw",
    "Vpmulhw",
    "Vpmulld",
    "Vpmullw",
    // task-215 (TLS): VEX 4-operand variable blends — jit test blend_and_cmpq_match_interp.
    "Vblendvpd",
    "Vblendvps",
    "Vpblendvb",
    // task-215 (TLS): EVEX qword compare→mask — jit test blend_and_cmpq_match_interp.
    "Vpcmpeqq",
    "Vpcmpgtq",
    // task-215 (TLS): per-element variable shifts — jit test variable_shifts_match_interp.
    "Vpsllvd",
    "Vpsllvq",
    "Vpsllvw",
    "Vpsravd",
    "Vpsravq",
    "Vpsravw",
    "Vpsrlvd",
    "Vpsrlvq",
    "Vpsrlvw",
    "Vpor",
    "Vpord",
    "Vporq",
    "Vpermilpd", // task-215: native+jit tests (native_vpermil/vpermil_imm_match_interp)
    "Vpermilps", // task-215: native+jit tests (native_vpermil/vpermil_imm_match_interp)
    "Vpshufb",
    "Vpshufd",
    // task-210: VEX.128 vpsign — see the psign* coverage note above.
    "Vpsignb",
    "Vpsignd",
    "Vpsignw",
    "Vprold", // task-209
    "Vprolq", // task-209
    "Vpslld",
    "Vpslldq",
    "Vpsllq",
    "Vpsllw",
    "Vpsrad",
    "Vpsraq", // task-215: native+jit tests (native_masked_shift/masked_shift_512_match_interp)
    "Vpsraw",
    "Vpsrld",
    "Vpsrldq",
    "Vpsrlq",
    "Vpsrlw",
    "Vpsubb",
    "Vpsubd",
    "Vpsubq",
    "Vpsubw",
    "Vpternlogd",
    "Vpternlogq",
    "Vptest",
    "Vpunpckhbw",
    "Vpunpckhdq",
    "Vpunpckhqdq",
    "Vpunpckhwd",
    "Vpunpcklbw",
    "Vpunpckldq",
    "Vpunpcklqdq",
    "Vpunpcklwd",
    "Vpxor",
    "Vpxord",
    "Vpxorq",
    "Vrndscalesd",
    "Vrndscaless",
    // task-242: VEX.128 ROUND family — hand-written differential (vex_eq_sse against the
    // corpus-trusted SSE round lowering; Unicorn's QEMU drops VEX.vvvv so it can't decode
    // the 3-operand scalar forms). The exact Mono blocker `vroundsd $0x9` is covered too.
    "Vroundpd",
    "Vroundps",
    "Vroundsd",
    "Vroundss",
    "Vshuff32x4", // task-209
    "Vshuff64x2", // task-209
    "Vsqrtsd",
    "Vsqrtss",
    "Vsubpd",
    "Vsubps",
    "Vsubsd",
    "Vsubss",
    "Vucomisd",
    "Vucomiss",
    "Vxorpd",
    "Vxorps",
    "Vzeroall",
    "Vzeroupper",
    "Wait",
    "Xadd",
    "Xchg",
    "Xorpd",
    "Xorps",
];

/// Every lifted mnemonic must be fuzzed or allowlisted (task-187). A new lift with
/// neither trips this — the offenders are named, with the fix.
#[test]
fn every_lifted_op_has_correctness_coverage() {
    let lifted = lifted_mnemonics();
    let fuzzed: BTreeSet<String> = FUZZER_COVERED.iter().map(|s| s.to_string()).collect();
    let allowed: BTreeSet<String> = ALLOWLIST.iter().map(|s| s.to_string()).collect();

    let uncovered: Vec<String> = lifted
        .iter()
        .filter(|m| !fuzzed.contains(*m) && !allowed.contains(*m))
        .cloned()
        .collect();

    assert!(
        uncovered.is_empty(),
        "coverage ratchet (task-187): {} newly-lifted mnemonic(s) have NO correctness \
         coverage — neither a fuzzer-menu entry (fuzz.rs) nor an ALLOWLIST entry:\n  {}\n\n\
         Fix: add a `FuzzInsn` generator (and its mnemonic to FUZZER_COVERED), OR — as a \
         last resort — add an explicit ALLOWLIST entry with a reason in \
         x86jit-tests/tests/coverage_ratchet.rs.",
        uncovered.len(),
        uncovered.join("\n  "),
    );
}

/// Guard: neither list may name a mnemonic that isn't actually lifted (a stale entry
/// left behind after a lift was removed, or a typo). Keeps the lists honest.
#[test]
fn coverage_lists_have_no_stale_entries() {
    let lifted = lifted_mnemonics();
    let stale: Vec<&str> = FUZZER_COVERED
        .iter()
        .chain(ALLOWLIST.iter())
        .copied()
        .filter(|m| !lifted.contains(*m))
        .collect();
    assert!(
        stale.is_empty(),
        "stale coverage-list entries (not lifted — remove them):\n  {}",
        stale.join("\n  "),
    );
}
