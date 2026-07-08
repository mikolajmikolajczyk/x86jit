---
id: TASK-168.3
title: 'AVX-3: AVX2 permute/broadcast/insert specials (glibc string routines)'
status: In Progress
assignee: []
created_date: '2026-07-08 15:21'
updated_date: '2026-07-08 16:59'
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
Common AVX2 specials done (interp + cranelift, jit==interp tested): vpbroadcast{b,w,d,q} reg+mem 128/256 (861f65e); vinserti128/vextracti128 (861f65e); 256-bit vpshufb per-lane + VEX packed shifts vpsll/vpsrl/vpsra w/d/q 128+256 (d6ee3cd). REMAINING: cross-lane permutes vpermq/vpermd/vperm2i128, vpalignr-256 (less common — memcpy tails, hashers). AC verification (glibc AVX2 strlen/strcmp/memchr) is gated on 168.4 (advertise AVX so glibc IFUNC picks the AVX paths) OR a dedicated unconditionally-AVX2 test binary. 168.4 is RISKY: advertising AVX2 makes the whole glibc corpus switch to AVX routines that may use ops not yet lifted -> must run full corpus + fix gaps in a loop before landing.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
