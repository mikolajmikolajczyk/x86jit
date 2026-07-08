---
id: TASK-168
title: 'M8-SIMD: VEX/AVX + AVX2 lifting (run x86-64-v3 host binaries)'
status: Done
assignee: []
created_date: '2026-07-08 15:11'
updated_date: '2026-07-08 17:49'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
ordinal: 177000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
x86jit's lifter is SSE-era: no VEX prefix decode, no AVX/AVX2. Modern optimized distros (CachyOS = x86-64-v3) build every /usr/bin binary with AVX — e.g. /usr/bin/echo has 55x vmovdqu + vpxor + vzeroupper — so they trap immediately on 'UNKNOWN INSTRUCTION c5 f9 ef c0' (vpxor xmm,xmm,xmm, VEX.128). Surfaced by x86jit-cli running host binaries. Baseline/SSE-only binaries already run; AVX is the gate to running a v3-optimized host's binaries. Scope: 2-/3-byte VEX prefix decode (c5/c4), the AVX-128/256 forms of the SSE ops already lifted (mov*/pxor/pcmpeq*/padd*/pshufb/…), vzeroupper/vzeroall, and YMM state (256-bit vector regs) in CpuState + both backends. CPUID would then advertise AVX (revisit decision-2's SSE mask). Big but well-scoped; belongs to m8-simd.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 VEX-encoded AVX-128/256 forms of the currently-lifted SSE instructions decode and execute (interp == jit == unicorn); vzeroupper is a no-op-safe; YMM registers modeled
- [ ] #2 A CachyOS-style AVX-built coreutils binary (e.g. /usr/bin/echo) runs to completion under x86jit-cli on both engines
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE. Full VEX/AVX + AVX2 lifting shipped across 168.1-168.4: VEX decode + AVX-128 (u128 IR), YMM 256-bit state + AVX-256 forms + vzeroupper, AVX2 permute/broadcast/insert specials + cross-lane permutes, and CPUID advertise AVX/AVX2 (+xgetbv/vptest). x86-64-v3 host binaries run: whole local real-binary corpus passes 3-way (native==interp==jit) with glibc/Go on AVX2 paths. Full non-fuzz suite 272/272 green. decision-11 records the advertise. Next: AVX-512/EVEX (future, CachyOS /usr/bin are v4).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
