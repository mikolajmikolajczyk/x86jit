---
id: TASK-168.3
title: 'AVX-3: AVX2 permute/broadcast/insert specials (glibc string routines)'
status: In Progress
assignee: []
created_date: '2026-07-08 15:21'
updated_date: '2026-07-08 17:29'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies:
  - TASK-168
parent_task_id: TASK-168
ordinal: 180000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The AVX2 ops glibc's string/memory IFUNC routines use beyond the basics: vpbroadcastb/d/q, vperm2i128/vpermq/vpermd, vinserti128/vextracti128, vpblendvb, vpmovmskb-256, vpalignr-256. Enables AVX2 strlen/strcmp/memchr/memcpy.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 glibc's AVX2 string routines (strlen/strcmp/memchr) execute correctly under x86jit interp == jit == unicorn
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Common AVX2 specials done (interp + cranelift, jit==interp tested): vpbroadcast{b,w,d,q} reg+mem 128/256 (861f65e); vinserti128/vextracti128 (861f65e); 256-bit vpshufb per-lane + VEX packed shifts vpsll/vpsrl/vpsra w/d/q 128+256 (d6ee3cd). Cross-lane permutes DONE: vpermq (imm), vpermd (reg control, cranelift via stack-gather), vperm2i128/f128 (lane select + zero), vpalignr 256 (per-lane) + VEX.128; reg forms, mem sources deferred (mirrors vinserti128). jit==interp test avx2_cross_lane_permutes_match_interp; corpus green. AC verification (glibc AVX2 strlen/strcmp/memchr) still gated on 168.4 (advertise AVX so glibc IFUNC picks AVX paths). 168.4 is RISKY: advertising AVX2 switches the whole glibc corpus to AVX routines that may use ops not yet lifted -> must run full corpus + fix gaps in a loop before landing.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
