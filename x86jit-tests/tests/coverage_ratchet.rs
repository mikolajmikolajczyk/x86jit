//! Coverage ratchet: the compat map tracks *presence* (does an op lift), but nothing
//! forces a newly-lifted op to have a *correctness* test. This ratchet closes that
//! gap. It asserts:
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
    // SSE2 saturating add/sub, rounding average, signed packs, pmaddwd (VBin).
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
/// reason. Seeded with exactly the current `lifted − fuzzer_covered` set, so the test
/// passes today.
const ALLOWLIST: &[&str] = &[
    "Addpd",
    "Addps",
    "Addsd",
    "Addss",
    // SSE3 lane-combining packed float — hand-written differential
    // (hadd_hsub_addsub_matches_unicorn / vex128_hadd_hsub_addsub / *_mem_*).
    "Addsubpd",
    "Addsubps",
    "Andnpd",
    "Andnps",
    "Andpd",
    "Andps",
    "Bextr",
    // SSE4.1 imm8 static blends — differential blendi_sse_matches_unicorn (SSE ==
    // hardware) + jit blend_imm8_match_interp (jit == interp incl. m128, dst==src2 alias).
    "Blendpd",
    "Blendps",
    "Blendvpd",
    "Blendvps",
    // --- Ops the ratchet could not see until the coverage probe learned to encode a
    // memory operand. None of them is newly lifted; the probe filed every pure-memory
    // and memory-alternative form as `unencodable`, so the whole class was invisible to
    // this test. Each line below says where its coverage actually is, or that it has none.

    // x87 memory-operand arithmetic and the environment/control ops — hand-written
    // differential: x87_body, x87_fldenv_body (x86jit-tests/src/snippets.rs) and the
    // x87 integer-arithmetic tests. `Fstcw`/`Fstenv` are the waiting aliases of the
    // `Fn*` forms and lift through the same path those tests drive.
    "Fiadd",
    // `ficom`/`ficomp` report ONLY through the status-word condition codes
    // C0/C2/C3, and the differential snapshot does not carry the status word — so a
    // fuzzer entry would compare two engines on a value neither of them exposes and
    // report agreement no matter what. Covered instead where the effect is visible:
    // `x87_exception_flags::ficom_sets_the_condition_codes` runs all four relations plus
    // the NaN case against the REAL CPU, reading the codes back through `fnstsw`.
    "Ficom",
    "Ficomp",
    "Fidiv",
    "Fidivr",
    "Fild",
    "Fimul",
    "Fistp",
    "Fisttp",
    "Fisub",
    "Fisubr",
    "Fldcw",
    "Fldenv",
    "Fnstcw",
    "Fnstenv",
    "Fstcw",
    "Fstenv",
    "Fxrstor",
    "Fxrstor64",
    "Fxsave",
    "Fxsave64",
    // `lddqu` — differential lddqu_loads_like_movdqu (jit == interp, unaligned source).
    "Lddqu",
    // Address arithmetic and byte-swapping loads: covered many times over by the
    // addressing vectors and the movbe tests.
    "Lea",
    "Movbe",
    // 64-bit-half moves. The SSE forms are driven by sse_half_body; the VEX forms share
    // the same lift and are not separately named by a test.
    "Movhpd",
    "Movhps",
    "Movlpd",
    "Movlps",
    "Vmovhpd",
    "Vmovhps",
    "Vmovlpd",
    "Vmovlps",
    // Non-temporal stores/loads — the streaming hint is a no-op in a
    // coherent model, so these lower like their ordinary counterparts, with tests.
    "Movntdq",
    "Movntdqa",
    "Movnti",
    "Movntpd",
    "Movntps",
    "Vmovntdq",
    "Vmovntdqa",
    "Vmovntpd",
    "Vmovntps",
    // Prefetch hints: architecturally no-ops, nothing to compare.
    "Prefetchnta",
    "Prefetcht0",
    "Prefetcht1",
    "Prefetcht2",
    // VEX 128-bit broadcasts. The EVEX family has tests; these VEX forms go
    // through the same lowering but no test names them. A real gap, small.
    "Vbroadcastf128",
    "Vbroadcastf32x4",
    "Vbroadcastf64x2",
    "Vbroadcasti128",
    "Vbroadcasti32x4",
    "Vbroadcasti64x2",
    // Masked vector moves — differential vmaskmovps_wild_bytes and the native leg.
    "Vmaskmovpd",
    "Vmaskmovps",
    // MXCSR load/store. NO COVERAGE, and deliberately so: the register is modelled as
    // storage and does not govern vector arithmetic — rounding control does not reach
    // the ops and the exception flags are not raised. See deferred.md, "MXCSR and
    // vector FP flag semantics". Testing these against hardware would fail today, and
    // it would be failing for the deferred reason rather than for these instructions.
    "Ldmxcsr",
    "Stmxcsr",
    "Vldmxcsr",
    "Vstmxcsr",
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
    // packed float↔int converts — hand-written differential
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
    "Cvttpd2dq", // packed truncating convert
    "Cvttps2dq", // packed truncating convert
    "Cvttsd2si",
    "Cvttss2si",
    "Cwd",
    "Cwde",
    "Db",
    "Dd",
    // SSE4.1 double-precision dot product — differential dppd_sse_matches_unicorn
    // (SSE == hardware) + jit dp_match_interp (jit == interp via shared dppd helper).
    "Dppd",
    // SSE4.1 single-precision dot product — jit test sse41_dpps_match_interp (jit
    // == interp via shared dpps helper) + native_dpps_matches_interp (bit-exact vs CPU, NaN).
    "Dpps",
    "Div",
    "Divpd",
    "Divps",
    "Divsd",
    "Divss",
    "Dq",
    "Dw",
    "Emms",  // MMX↔x87 bridge; emms is a no-op in our model
    "F2xm1", // x87 transcendental
    "Fabs",
    "Fadd",
    "Faddp",
    "Fchs",
    // x87 unit management (fninit/fnclex + the waiting forms finit/fclex): reset the
    // control/status/tag words / clear exception flags — hand-written interpreter test
    // (x86jit-tests/tests/interpreter.rs), no data operands to fuzz.
    "Fclex",
    "Fcos", // x87 transcendental
    "Fcomi",
    "Fcomip",
    "Fdiv",
    "Fdivp",
    "Fdivr",
    "Fdivrp",
    "Finit", // x87 unit reset — see Fclex/Fnclex/Fninit
    "Fld",
    "Fld1",
    "Fldz",
    "Fmul",
    "Fmulp",
    "Fnclex", // x87 clear exception flags — see Fclex
    "Fninit", // x87 reinit — see Fclex
    "Fnstsw",
    "Fpatan", // x87 transcendental
    "Fprem",
    "Fptan",   // x87 transcendental
    "Fsin",    // x87 transcendental
    "Fsincos", // x87 transcendental
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
    "Fyl2x",   // x87 transcendental
    "Fyl2xp1", // x87 transcendental
    // SSE3 horizontal add/sub — hand-written differential.
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
    // SSE4.1 lane insert + zero mask — jit test sse41_insertps_match_interp (jit ==
    // interp, inline codegen shuffle) + native_insertps_matches_interp (bit-exact vs CPU).
    "Insertps",
    "Jmp",
    // LAHF/SAHF (see "Sahf"). Kept OUT of the fuzzer menu on purpose — the
    // menu builds multi-instruction sequences, so a `lahf` following a shift or a
    // multiply would materialize our arbitrary choice for an ARCHITECTURALLY UNDEFINED
    // AF/PF into AH, turning a waived flag difference into a register difference that
    // no `dont_care` mask can cover. `lockstep.rs` excludes the pair for the same
    // reason. Covered instead by hand-written snippets: differential
    // sahf_lahf_round_trip_matches_unicorn / sahf_leaves_overflow_untouched_vs_unicorn
    // / lahf_captures_computed_flags_vs_unicorn (interp == hardware), plus jit
    // lahf_sahf_round_trip / sahf_preserves_overflow / lahf_captures_computed_flags
    // (jit == interp).
    "Lahf",
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
    "Movdq2q", // MMX↔XMM bridge
    "Movdqa",
    "Movdqu",
    "Movhlps",
    "Movlhps",
    // VEX.128 3-operand move-packed-half — vex_eq_sse (vmov_lhps_hlps_vex_eq_sse)
    // + dst==src2 alias oracle (vmovlhps_dst_aliases_src2) + jit vmovlhps_vmovhlps_match_interp.
    "Vmovhlps",
    "Vmovlhps",
    // SSE3 lane-duplicating moves (fixed dword shuffles) + VEX.128 — differential
    // movdup_family_match_unicorn + vmovdup_family_vex_eq_sse + jit movdup_family_match_interp.
    "Movddup",
    "Movshdup",
    "Movsldup",
    "Vmovddup",
    "Vmovshdup",
    "Vmovsldup",
    // packed-float sign-mask extract — differential movmsk_ps_pd_match_unicorn
    // (interp vs CPU: all-neg/all-pos/mixed) + jit movmsk_ps_pd_match_interp.
    "Movmskpd",
    "Movmskps",
    "Movq",
    "Movq2dq", // MMX↔XMM bridge
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
    // SSE4.2 string compare → mask in XMM0 — jit test sse42_pcmpstrm_match_interp
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
    // SSSE3 packed-integer horizontal add/sub — hand-written differential
    // (phadd_phsub_matches_unicorn / phadd_phsub_memory_source_* / vex128_phadd_phsub).
    "Phaddd",
    "Phaddsw",
    "Phaddw",
    "Phsubd",
    "Phsubsw",
    "Phsubw",
    // VEX v3 converts + movmsk/test/round/dpps ymm + horizontal/sign ymm and
    // SSE4.1 specialists. Covered by dedicated jit==interp tests in jit.rs
    // (vex256_* / f16c_converts_match_interp / sse41_avx_specialists_match_interp /
    // pcmpestr64_match_interp) and bit-exact native-oracle tests in native.rs
    // (native_vex256_width_converts_ / native_f16c_converts_ / native_specialists_and_test_).
    "Mpsadbw",
    "Vmpsadbw",
    "Phminposuw",
    "Vphminposuw",
    "Vdpps",
    "Vtestps",
    "Vtestpd",
    "Vcvtph2ps",
    "Vcvtps2ph",
    "Pcmpestri64",
    "Pcmpestrm64",
    "Vpcmpestri64",
    "Vpcmpestrm64",
    // SSE2 psadbw — hand-written differential (psadbw_matches_unicorn /
    // psadbw_memory_source_matches_unicorn).
    "Psadbw",
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
    "Pmuludq", // native+jit tests (native_vpmuludq/vpmuludq_match_interp)
    // caddy HTTPS: SSE word blend — jit test pblendw_match_interp.
    "Pblendw",
    // TLS: packed multiplies — jit test packed_muls_match_interp.
    "Pmuldq",
    "Pmulhuw",
    "Pmulhw",
    "Pmullw",
    "Pop",
    "Pshufb",
    "Pshufhw",
    "Pshuflw",
    // SSSE3 psign — pure element-wise codegen, covered by the dedicated
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
    // SAL is the /6 encoding alias of SHL (identical semantics, same lift
    // path). Covered by `sal_alias_matches_interp` in jit.rs; the fuzzer menu only
    // emits the /4 SHL form, so credit SAL here rather than in FUZZER_COVERED.
    "Sal",
    // See the "Lahf" entry above — same pair, same reason.
    "Sahf",
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
    // SSE float unpacks — reuse the integer interleave helper at the matching lane
    // width; differential vunpck_vex_eq_sse + jit vunpck_match_interp + native bit-exact sweep.
    "Unpckhpd",
    "Unpckhps",
    "Unpcklpd",
    "Unpcklps",
    "Vaddpd",
    "Vaddps",
    "Vaddsd",
    "Vaddss",
    // VEX.128 addsub — hand-written differential (vex_eq_sse).
    "Vaddsubpd",
    "Vaddsubps",
    "Valignd",
    "Valignq",
    "Vandnpd",
    "Vandnps",
    "Vandpd",
    "Vandps",
    // broadcast family — covered by native_broadcast_lane_matches_interp +
    // broadcast_lane_variants_match_interp (lane forms) and the scalar-broadcast lift.
    "Vbroadcastf32x2",
    "Vbroadcasti32x2",
    "Vbroadcastsd",
    "Vbroadcastss",
    // VEX `vcmp{ss,sd,ps,pd}` (VEX.128 + VEX.256) — the 3-operand
    // float-compare-with-predicate family. QEMU mis-decodes VEX 3-operand ops, so the
    // JIT==interp tests (vcmp_vex_match_interp / survival_vcmp / vcmpltss_exact_bytes_lifts)
    // are the oracle, with vcmp_vex128_eq_sse asserting the VEX.128 lowering equals the
    // unicorn-validated legacy SSE compare.
    "Vcmppd",
    "Vcmpps",
    "Vcmpsd",
    "Vcmpss",
    "Vcomisd",
    "Vcomiss",
    // VEX.128 packed converts — cvt_packed_vex128_matches_sse (VEX == the
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
    "Vcvttpd2dq", // packed truncating convert
    "Vcvttps2dq", // packed truncating convert
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
    // `vextractps r/m32, xmm, imm8` — hand-written differential
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
    // FMA alternating-sign family: covered by jit `fma_addsub_subadd_match_interp`
    // (jit == interp, xmm+ymm, reg+mem, NaN lane) + native `native_fma_addsub_matches_interp`
    // (bit-exact vs real CPU, fused rounding + per-lane even/odd sign).
    "Vfmaddsub132pd",
    "Vfmaddsub132ps",
    "Vfmaddsub213pd",
    "Vfmaddsub213ps",
    "Vfmaddsub231pd",
    "Vfmaddsub231ps",
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
    // FMA alternating-sign family: see the vfmaddsub note above.
    "Vfmsubadd132pd",
    "Vfmsubadd132ps",
    "Vfmsubadd213pd",
    "Vfmsubadd213ps",
    "Vfmsubadd231pd",
    "Vfmsubadd231ps",
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
    // VEX.128 horizontal add/sub — hand-written differential (vex_eq_sse).
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
    // VEX.128 3-operand `vinsertps` — differential vinsertps_reg/mem_vex_eq_sse
    // (VEX == trusted SSE insertps) + vinsertps_wild_bytes (exact c4 e3 79 21 d1 10)
    // + jit vinsertps_match_interp (jit == interp incl. m32, dst==src2 alias, upper-zeroing).
    "Vinsertps",
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
    "Vmovmskpd", // VEX.128 sign-mask; shares the movmsk lowering
    "Vmovmskps", // same
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
    // AVX-512 masked EVEX ops: jit==interp + native bit-exact in native.rs/jit.rs.
    "Vpblendmd",
    "Vpblendmq",
    "Vpblendd", // native+jit tests (native_vpblendd/vpblendd_match_interp)
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
    "Vpconflictd", // AVX-512 EVEX misc
    "Vpconflictq", // AVX-512 EVEX misc
    "Vperm2f128",
    "Vperm2i128",
    "Vpermd",
    "Vpermi2d",
    "Vpermi2q",
    "Vpermi2w",
    "Vpermpd", // imm8 4-qword permute (jit test vpermq_mem_imm_match_interp)
    "Vpermq",
    "Vpermt2d",
    "Vpermt2q",
    "Vpermt2w",
    "Vpextrb",
    "Vpextrd",
    "Vpextrq",
    "Vpextrw",
    // VEX.128 packed-integer horizontal add/sub — hand-written differential
    // (vex128_phadd_phsub via vex_eq_sse; incl the blocker vphaddd xmm0,xmm0,xmm0).
    "Vphaddd",
    "Vphaddsw",
    "Vphaddw",
    "Vphsubd",
    "Vphsubsw",
    "Vphsubw",
    // VEX.128 vpsadbw — hand-written differential (vex128_psadbw via vex_eq_sse;
    // incl. the dst==src1 shape vpsadbw xmm4,xmm4,xmm0).
    "Vpsadbw",
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
    "Vplzcntd", // AVX-512 EVEX misc
    "Vplzcntq", // AVX-512 EVEX misc
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
    "Vpmuludq", // native+jit tests (native_vpmuludq/vpmuludq_match_interp)
    // TLS: packed multiplies — jit test packed_muls_match_interp.
    "Vpmuldq",
    "Vpmulhuw",
    "Vpmulhw",
    "Vpmulld",
    "Vpmullw",
    // TLS: VEX 4-operand variable blends — jit test blend_and_cmpq_match_interp.
    // (m128 src2 form: jit vblendv_memory_match_interp + differential
    // vblendv_memory_source_vex_eq_sse + vblendvps_wild_bytes.)
    "Vblendvpd",
    "Vblendvps",
    "Vpblendvb",
    // VEX 3-operand imm8 static blends — differential vblendi_vex_eq_sse (VEX ==
    // trusted SSE) + jit blend_imm8_match_interp (jit == interp incl. m128, dst==src2 alias).
    "Vblendpd",
    "Vblendps",
    // VEX 3-operand dot products — differential vdp_vex_eq_sse (VEX == trusted SSE
    // dpps/dppd) + jit dp_match_interp (jit == interp; native_vex_float_cluster bit-exact).
    "Vdppd",
    "Vdpps",
    // TLS: EVEX qword compare→mask — jit test blend_and_cmpq_match_interp.
    "Vpcmpeqq",
    "Vpcmpgtq",
    // TLS: per-element variable shifts — jit test variable_shifts_match_interp.
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
    "Vpermilpd", // native+jit tests (native_vpermil/vpermil_imm_match_interp)
    "Vpermilps", // native+jit tests (native_vpermil/vpermil_imm_match_interp)
    "Vpermps",   // native+jit tests (native_avx2_lane_shuffles/avx2_vpermps_ymm)
    "Vpshufb",
    "Vpshufd",
    "Vpshufhw", // native+jit tests (native_avx2_lane_shuffles/avx2_vpshufhwlw_ymm)
    "Vpshuflw", // native+jit tests (native_avx2_lane_shuffles/avx2_vpshufhwlw_ymm)
    // VEX.128 vpsign — see the psign* coverage note above.
    "Vpsignb",
    "Vpsignd",
    "Vpsignw",
    "Vprold", // AVX-512 EVEX misc
    "Vprolq", // AVX-512 EVEX misc
    "Vpslld",
    "Vpslldq",
    "Vpsllq",
    "Vpsllw",
    "Vpsrad",
    "Vpsraq", // native+jit tests (native_masked_shift/masked_shift_512_match_interp)
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
    // reciprocal (exact-IEEE 1.0/x) — differential/jit reuse the interp lowering;
    // native_vex_rcp_rsqrt_within_tolerance validates the SDM 1.5*2^-12 rel-error bound.
    "Vrcpps",
    "Vrcpss",
    "Vrndscalesd",
    "Vrndscaless",
    // VEX.128 ROUND family — hand-written differential (vex_eq_sse against the
    // corpus-trusted SSE round lowering; Unicorn's QEMU drops VEX.vvvv so it can't decode
    // the 3-operand scalar forms). The reported blocker `vroundsd $0x9` is covered too.
    "Vroundpd",
    "Vroundps",
    "Vroundsd",
    "Vroundss",
    // reciprocal-sqrt (exact-IEEE 1.0/sqrt(x)) — the concrete reported blocker
    // (vrsqrtss_wild_bytes: c5 fa 52 d0); native tolerance test validates the bound.
    "Vrsqrtps",
    "Vrsqrtss",
    "Vshuff32x4", // AVX-512 EVEX misc
    "Vshuff64x2", // AVX-512 EVEX misc
    // VEX 3-operand shuffles — differential vshuf_vex_eq_sse + jit vshuf_match_interp
    // + native bit-exact sweep. Register + m128 src2 + dst==src2 alias + VEX upper-zero.
    "Vshufpd",
    "Vshufps",
    // VEX packed sqrt (2-operand) — differential vsqrt_vex_eq_sse + native sweep.
    "Vsqrtpd",
    "Vsqrtps",
    "Vsqrtsd",
    "Vsqrtss",
    "Vsubpd",
    "Vsubps",
    "Vsubsd",
    "Vsubss",
    "Vucomisd",
    "Vucomiss",
    // VEX float unpacks — reuse lift_vunpack_avx (reg/m128 + VZeroUpper); differential
    // vunpck_vex_eq_sse + jit vunpck_match_interp + native bit-exact sweep.
    "Vunpckhpd",
    "Vunpckhps",
    "Vunpcklpd",
    "Vunpcklps",
    "Vxorpd",
    "Vxorps",
    "Vzeroall",
    "Vzeroupper",
    "Wait",
    "Xadd",
    "Xchg",
    "Xorpd",
    "Xorps",
    // VEX packed-int sweep + SSSE3 pmulhrsw/pmaddubsw — covered by the
    // hand-written jit==interp test avx2_packed_int_sweep_match_interp (jit.rs) and the
    // native-oracle native_packed_int_sweep_matches_interp (native.rs), which exercise
    // every form at xmm+ymm, reg+mem, over saturation/rounding edges.
    "Pmaddubsw",
    "Pmulhrsw",
    "Vpaddsb",
    "Vpaddsw",
    "Vpaddusb",
    "Vpaddusw",
    "Vpavgb",
    "Vpavgw",
    "Vpmaddubsw",
    "Vpmaddwd",
    "Vpmaxsb",
    "Vpmaxsw",
    "Vpmaxuw",
    "Vpminsb",
    "Vpminsw",
    "Vpminuw",
    "Vpmulhrsw",
    "Vpsubsb",
    "Vpsubsw",
    "Vpsubusb",
    "Vpsubusw",
];

/// Every lifted mnemonic must be fuzzed or allowlisted. A new lift with
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
        "coverage ratchet: {} newly-lifted mnemonic(s) have NO correctness \
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
