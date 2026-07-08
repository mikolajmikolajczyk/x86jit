---
id: TASK-168.5.4
title: >-
  AVX-512 prerequisite: SSE4.2 pcmpistri/pcmpestri (+ SSE4.1
  pmovzx/blendv/pmulld/round/ptest)
status: To Do
assignee: []
created_date: '2026-07-08 19:19'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 187000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
SSE4.2 string-compare aggregation ops (pcmpistri[204]/pcmpestri) + the SSE4.1 gaps decision-2 dropped. Needed because advertising v2+ makes glibc IFUNC + inline code select them. Complex aggregation semantics. Priority 4.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
