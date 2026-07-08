---
id: TASK-168.3
title: 'AVX-3: AVX2 permute/broadcast/insert specials (glibc string routines)'
status: To Do
assignee: []
created_date: '2026-07-08 15:21'
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

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
