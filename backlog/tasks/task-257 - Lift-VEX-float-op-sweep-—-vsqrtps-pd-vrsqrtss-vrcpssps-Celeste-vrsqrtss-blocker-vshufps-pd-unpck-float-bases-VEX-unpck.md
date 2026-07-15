---
id: TASK-257
title: >-
  Lift VEX float-op sweep — vsqrtps/pd, vrsqrtss/vrcpss+ps (Celeste vrsqrtss
  blocker), vshufps/pd, unpck float bases + VEX unpck
status: Done
assignee: []
created_date: '2026-07-15 22:48'
updated_date: '2026-07-15 23:09'
labels:
  - m8-simd
dependencies: []
ordinal: 287000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Celeste (Mono+FNA, pervasively AVX/VEX) faults one 128-bit VEX float op at a time. Lift the missing xmm-only VEX float cluster that mechanically re-encodes SSE ops x86jit already supports, plus the genuinely-new reciprocal family the CONCRETE blocker (vrsqrtss, c5 fa 52 d0) needs. Groups: A packed sqrt (vsqrtps/vsqrtpd); B reciprocal (vrsqrtss/vrcpss scalar 3-op + vrsqrtps/vrcpps packed 2-op, new FloatUnOp::Rsqrt/Rcp, exact-IEEE semantics documented as an approximation of the hw ~12-bit estimate); C shuffles (vshufps/vshufpd 3-operand distinct-vvvv + m128); D float unpacks (SSE bases unpcklps/hps/lpd/hpd + VEX vunpck* reusing int-unpack helpers). 256-bit forms, FMA-addsub, vpermilps/pd, half-float, integer sat/avg ops are OUT of scope.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vrsqrtss (c5 fa 52 d0) decodes+runs, no UnknownInstruction; src.low=1.0 -> 1.0, upper 255:128 zeroed
- [x] #2 Group A/B/C/D ops decoded+lifted (xmm-only), all three tiers wired (decode/lift, interp, cranelift)
- [x] #3 rsqrt/rcp within SDM rel-error 3.66e-4 vs exact 1/x and 1/sqrt(x); interp==jit exact-IEEE
- [x] #4 differential vex_eq_sse + native bit-exact sweep (sqrt/shuf/unpck) green; coverage map + ratchet updated
- [x] #5 clippy -D warnings clean, fmt clean, full nextest suite green
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-16. Lifted the xmm-only VEX float sweep across all three tiers (lift/interp/cranelift-jit).

Groups:
A vsqrtps/vsqrtpd — new lift_vfloat_unary_packed (2-op, no vvvv); reg + m128 via new VFloatUnaryM IR.
B vrsqrtss/vrcpss (scalar 3-op, m32) + vrsqrtps/vrcpps (packed 2-op) — new FloatUnOp::Rsqrt/Rcp = EXACT IEEE 1.0/sqrt(x) & 1.0/x (interp apply_un_f32; jit emit_funary builds 1.0 matching scalar/vector type). F64 rsqrt/rcp unreachable! (no encodings). lift_vfloat_unary_scalar extended with the _M (m32/m64) path.
C vshufps/vshufpd — new lift_vshufps (3-op distinct-vvvv), reuses VShufps + new VShufpsM (m128). shufpd imm expanded to 4x2-bit dword form.
D SSE bases unpcklps/hps/lpd/hpd (reuse lift_vunpack) + VEX vunpcklps/hps/lpd/hpd (reuse lift_vunpack_avx, which already appends VZeroUpper — not doubled).

New IR: VFloatUnaryM, VShufpsM. New FloatUnOp: Rsqrt, Rcp.
256-bit _ymm_ forms out of scope (reg_xmm returns None → clean deferral).

The concrete Celeste blocker vrsqrtss (c5 fa 52 d0) decodes+runs: src.low 1.0 → 1.0, upper from op1, 255:128 zeroed (vrsqrtss_celeste_wild_bytes).

Tests: differential vsqrt/vshuf/vunpck_vex_eq_sse (VEX==trusted SSE) + vrsqrtss_celeste_wild_bytes; jit vsqrt/vshuf/vunpck/vrcp_rsqrt_match_interp (jit==interp incl m128/m32, dst==src2 alias, upper-zero); native_vex_float_sweep_matches_interp (bit-exact vs host AVX) + native_vex_rcp_rsqrt_within_tolerance (max rel-err 0.0, SDM bound 3.66e-4). coverage map + ratchet allowlist updated.

Gates: full nextest 757 passed / 3 skipped; clippy -D warnings clean; fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
