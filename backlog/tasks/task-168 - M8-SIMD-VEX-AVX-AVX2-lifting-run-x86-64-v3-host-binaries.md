---
id: TASK-168
title: 'M8-SIMD: VEX/AVX + AVX2 lifting (run x86-64-v3 host binaries)'
status: To Do
assignee: []
created_date: '2026-07-08 15:11'
updated_date: '2026-07-08 15:41'
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
DISCOVERY (via x86jit-cli on CachyOS): the host's /usr/bin/* are built -march=x86-64-v4 = AVX-512 (EVEX, 62-prefix), not just AVX2. After 168.1 lifted VEX.128, /usr/bin/echo advanced from the vpxor(VEX) trap to '62 f1 7f 48 7f ...' = EVEX vmovdqu (AVX-512). So running v4 host binaries needs an EVEX/AVX-512 tier BEYOND 168.1-4 (VEX/AVX2). Consider a 168.5 for EVEX + K-mask regs + ZMM once AVX2 lands. Baseline/SSE + (with 168.x) VEX/AVX2 binaries run; full CachyOS /usr/bin needs AVX-512.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
