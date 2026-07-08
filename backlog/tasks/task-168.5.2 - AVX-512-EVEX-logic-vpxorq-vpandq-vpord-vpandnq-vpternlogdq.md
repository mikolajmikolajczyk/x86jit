---
id: TASK-168.5.2
title: 'AVX-512: EVEX logic vpxorq/vpandq/vpord/vpandnq + vpternlog{d,q}'
status: To Do
assignee: []
created_date: '2026-07-08 19:19'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 185000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
EVEX-encoded bitwise logic (vpxorq/vpandq/vpord/vpandnq, 128/256/512, masked+unmasked) — route like the EVEX 64-bit min/max did. Plus vpternlog{d,q}: 3-input arbitrary bitwise logic via an 8-bit truth table (new IR op). First post-advertise trap on /usr/bin/true is vpxorq. Priority 2.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
