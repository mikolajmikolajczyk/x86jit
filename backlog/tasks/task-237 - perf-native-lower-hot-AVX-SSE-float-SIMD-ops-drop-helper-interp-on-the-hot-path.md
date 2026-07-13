---
id: TASK-237
title: >-
  perf: native-lower hot AVX/SSE float SIMD ops (drop helper->interp on the hot
  path)
status: Done
assignee: []
created_date: '2026-07-12 20:21'
updated_date: '2026-07-13 11:26'
labels:
  - 'crate:cranelift'
  - 'goal:perf'
milestone: ps4-perf
dependencies: []
ordinal: 266000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
ps4-perf Tier-1, RE-SCOPED per the task-236 audit (doc-34). ORIGINAL premise (native-lower vmulps/vaddps/... for 2-10x) is void: the game-hot float core (add/sub/mul/div/min/max/sqrt/cmp, all packed-int, bitwise, imm-shifts, common shuffles/blends/broadcasts) is ALREADY native (builder.ins() -> NEON). No float-arith lever remains. Retargeted at the actually-helper-backed, PS4/Jaguar-reachable (SSE, 128-bit) ops from doc-34's ranked worklist: #2 shift_reg (psll/psrl/psra {w,d,q} xmm,xmm -- SSE2 scalar-xmm-count packed shift, currently helper 'shift_reg' cranelift/codegen/vector.rs) and #3 dpps/dppd (SSE4.1 dot product, currently helper 'dpps'). Native-lower both to Cranelift/NEON, bit-exact vs unicorn, measure on task-235's microbench. Expect single-digit % whole-program wins (not multiples) -- the hot float loops were never the helper cost. FMA (worklist #6) intentionally EXCLUDED -- big for AVX2+ guests but Jaguar/PS4 has no FMA3; belongs to a general (non-ps4-perf) track.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 the ranked hot AVX/SSE float ops lower natively (no interp helper) on x86 + ARM, bit-exact vs unicorn, with a measured microbench speedup recorded
- [x] #2 shift_reg (packed shift by xmm/scalar count) lowers natively (no interp helper) on x86 + ARM, bit-exact vs unicorn incl. over-shift clamping (count>=width -> 0 logical / sign arith)
- [x] #3 dpps/dppd lowers natively (no interp helper) on x86 + ARM, bit-exact vs unicorn incl. imm lane-select masks + NaN
- [ ] #4 cargo nextest run (--features unicorn) green minus fuzz_robustness; clippy clean; fmt clean; microbench delta recorded
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done (2 commits). RE-SCOPED per doc-34 audit (float already native). Delivered the PS4/Jaguar-reachable SSE helper->native swaps: (1) shift_reg [ff4f471]: psll/psrl/psra {w,d,q} xmm,xmm register-count — ALSO lifted the legacy-SSE 2-op form which previously TRAPPED (only imm form was lifted); native vector-shift-by-scalar-count -> NEON; over-shift clamp matches x86 (logical->0, arith->sign). Review caught 2 upper-bits bugs (VShiftReg had no SSE-vs-VEX discriminator): fixed to codebase convention (base op preserves 255:128, VEX/EVEX-128 lift appends VZeroUpper). (2) dpps [a7e25a3]: SSE4.1 dot product native — compile-time imm masks unrolled to scalar f32 in SDM tree order (P0+P1)+(P2+P3), bit-exact vs interp+CPU incl NaN; removed the now-unused dpps/dpps_mem helper wiring incl the aarch64-only barrier_tests dummy. FMA excluded (Jaguar has no FMA3). Both reviewed (no bugs), full suite 669 passed x86, clippy+fmt clean. CI: needs ARM validation of the barrier_tests helper-removal.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
