---
id: TASK-237
title: >-
  perf: native-lower hot AVX/SSE float SIMD ops (drop helper->interp on the hot
  path)
status: To Do
assignee: []
created_date: '2026-07-12 20:21'
labels:
  - 'crate:cranelift'
  - 'goal:perf'
milestone: ps4-perf
dependencies: []
ordinal: 266000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tier-1 BIG LEVER (doc-33). Replace helper->interp fallbacks for the hottest game-SIMD ops with native Cranelift lowering (-> ARM NEON on the primary target): AVX-128/SSE float first (vmulps/vaddps/vsubps/vdivps/vfmadd*/vshufps/vblendps/vcvt*/vminps/vmaxps/vsqrtps/vandps/vcmpps), then AVX-256 (2x NEON). 128-bit forms map ~directly to NEON. Each op: native-lower, validate bit-exact vs unicorn oracle, measure speedup on the game-shaped microbench (task for suite). Expect 2-10x on SIMD-heavy loops. Depends on the helper audit/ranking. Cross-arch caution: NEON is 128-bit; AVX-256 needs 2x, AVX-512 4x lanes.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 the ranked hot AVX/SSE float ops lower natively (no interp helper) on x86 + ARM, bit-exact vs unicorn, with a measured microbench speedup recorded
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
