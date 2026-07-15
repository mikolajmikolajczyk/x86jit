---
id: TASK-255
title: Lift VEX vinsertps (VEX.128.66.0F3A.W0 21) — 3-operand insert-scalar-single
status: Done
assignee: []
created_date: '2026-07-15 21:49'
updated_date: '2026-07-15 22:02'
labels:
  - m8-simd
dependencies: []
ordinal: 285000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Celeste faulted UnknownInstruction on c4 e3 79 21 d1 10 = VINSERTPS xmm2, xmm0, xmm1, 0x10 (VEX encoding of INSERTPS). Legacy SSE INSERTPS is already lifted; the VEX 3-operand form (distinct dst/src1 + upper-lane zeroing) is not. Reuse the existing VInsertPs/VInsertPsM IR + insertps semantics.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 VEX vinsertps decoded+lifted (reg + m32 forms)
- [x] #2 Wired in interp and cranelift JIT tiers
- [x] #3 Differential/vex_eq_sse tests pass incl. upper-lane zeroing and non-zero zmask/count_d variants
- [x] #4 clippy clean, fmt clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-16. Lifted VEX vinsertps (VEX.128.66.0F3A.W0 21) as new IR ops VInsertPs3/VInsertPsM3 (distinct merge base 'a' read before dst write → aliasing-safe) + trailing VZeroUpper for VEX.128 upper-lane clear. Wired decode (Mnemonic::Vinsertps), interp (exec_v_insert_ps3/_m3), cranelift JIT (emit_v_insert_ps3/_m3). Tests: differential vinsertps_reg/mem_vex_eq_sse + vinsertps_celeste_wild_bytes (exact c4 e3 79 21 d1 10), jit vinsertps_match_interp (incl. dst==src2 alias, m32, upper-zero), native_vinsertps_matches_interp (bit-exact vs real host AVX CPU). coverage_ratchet + compat map updated. Full suite 737 pass. Reused legacy insertps() semantics helper — no copied code.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
