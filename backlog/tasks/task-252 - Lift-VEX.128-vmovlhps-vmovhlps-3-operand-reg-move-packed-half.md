---
id: TASK-252
title: Lift VEX.128 vmovlhps / vmovhlps (3-operand reg move-packed-half)
status: Done
assignee: []
created_date: '2026-07-15 14:01'
updated_date: '2026-07-15 14:08'
labels:
  - lift
  - m8-simd
dependencies: []
ordinal: 282000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
unemups4 (Celeste/PS4) traps on `vmovlhps %xmm0,%xmm1,%xmm0` (VEX.128.0F 16 /r). Legacy Movlhps/Movhlps are lifted (VMoveHalf) but the VEX 3-operand forms Vmovlhps/Vmovhlps are absent from dispatch. Both are exactly 64-bit-lane unpacks: vmovlhps dst=[op1.lo,op2.lo]==vpunpcklqdq(op1,op2); vmovhlps dst=[op2.hi,op1.hi]==vpunpckhqdq(op2,op1). Reuse the existing VUnpackLow op (reads both srcs before writing dst, so the dst==src2 alias in the wild shape is safe) + VZeroUpper. No new IR op.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Vmovlhps/Vmovhlps VEX.128 lift; dst==src2 aliasing correct; bits 255:128 zeroed
- [ ] #2 differential vs Unicorn (or vex-vs-sse) + jit==interp; ratchet allowlist + coverage regen
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. Lifted VEX.128 vmovlhps/vmovhlps via existing VUnpackLow (lane=8) + VZeroUpper — no new IR op. vmovlhps=unpcklqdq(op1,op2); vmovhlps=unpckhqdq(op2,op1) swapped. VUnpackLow reads both srcs before writing dst → the wild dst==src2 alias (vmovlhps %xmm0,%xmm1,%xmm0, Celeste) is safe. Tests: vmov_lhps_hlps_vex_eq_sse (semantics vs SSE lowering), vmovlhps_dst_aliases_src2 (hand oracle + upper-zero), vmovlhps_vmovhlps_match_interp (jit==interp, dirty ymm_hi). Ratchet allowlist + coverage regen (v3 +2). 725/725, clippy+fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
