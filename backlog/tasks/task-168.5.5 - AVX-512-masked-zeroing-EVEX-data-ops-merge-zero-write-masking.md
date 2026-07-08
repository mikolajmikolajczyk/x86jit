---
id: TASK-168.5.5
title: 'AVX-512: masked/zeroing EVEX data ops (merge + zero write-masking)'
status: To Do
assignee: []
created_date: '2026-07-08 19:19'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 188000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The per-element masking subsystem: vmovdqu32/64{k}{z} + masked arithmetic/logic with merge (keep dst) vs zero semantics under a k write-mask (303 {k} sites in glibc). The one real subsystem among the AVX-512 gaps. Priority 5 (evex_is_masked currently -> unsupported for data ops).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
