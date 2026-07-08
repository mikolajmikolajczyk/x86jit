---
id: TASK-168.2
title: 'AVX-2: YMM 256-bit state + AVX-256 forms + real vzeroupper'
status: In Progress
assignee: []
created_date: '2026-07-08 15:21'
updated_date: '2026-07-08 16:10'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies:
  - TASK-168
parent_task_id: TASK-168
ordinal: 179000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Widen vector state xmm:[u128;16] -> ymm (256-bit) across CpuState and both backends (interp + cranelift), lift the VEX.256 (L=1) forms of the AVX-1 op set, and make vzeroupper actually zero bits 255:128. Needed for real -mavx2 (256-bit) code. Depends on AVX-1's VEX decode.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 256-bit vmovdqu/vpxor/vpcmpeqb/vpand.../vpsubb lift + execute interp == jit == unicorn on YMM; vzeroupper zeroes the upper halves; XSAVE-relevant state consistent
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
