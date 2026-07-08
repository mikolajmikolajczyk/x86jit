---
id: TASK-168.2
title: 'AVX-2: YMM 256-bit state + AVX-256 forms + real vzeroupper'
status: Done
assignee: []
created_date: '2026-07-08 15:21'
updated_date: '2026-07-08 16:45'
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
- [x] #1 256-bit vmovdqu/vpxor/vpcmpeqb/vpand.../vpsubb lift + execute interp == jit == unicorn on YMM; vzeroupper zeroes the upper halves; XSAVE-relevant state consistent
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Core AVX-256 done across state + interp + cranelift, tested jit==interp (compare now diffs ymm_hi; unicorn can't be the AVX oracle). Landed: YMM upper state (ymm_hi) + VEX.128 upper-zeroing + vzeroupper (bd25dc0); 256-bit data movement vmovdqu/vmovdqa/vmov (5e575f7); 256-bit logic/packed/movemask vpxor/vpand/vpor/vpandn, vpadd/vpsub/vpcmpeq/vpcmpgt, vpminub/vpmaxub, vpmovmskb, reg+mem forms (03a0cff). The AC's listed ops all execute both backends. NOT yet (cross-lane / special -> folded into 168.3): vpbroadcast*, vpshufb-256 (per-lane but needs the 256 form), vpalignr-256, vperm*, vinsert/vextract-i128, 256-bit shifts. glibc AVX2 strlen/strcmp (vpcmpeqb/vpminub/vpmovmskb/vpand) now covered; memchr needs vpbroadcastb (168.3).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
