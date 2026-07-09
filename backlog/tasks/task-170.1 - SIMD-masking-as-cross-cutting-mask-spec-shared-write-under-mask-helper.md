---
id: TASK-170.1
title: 'SIMD: masking as cross-cutting mask-spec + shared write-under-mask helper'
status: Done
assignee: []
created_date: '2026-07-08 20:24'
updated_date: '2026-07-08 21:59'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:refactor'
  - seq-2
dependencies: []
parent_task_id: TASK-170
ordinal: 191000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Design + implement k-writemask (k1-k7) + merge/zero as a small mask spec carried by maskable ops, applied by ONE shared 'commit vector result under mask' routine in interp and cranelift. Prereq for 168.5.5 (masked EVEX data ops) so masking doesn't multiply the IR. Needs a decision doc on the representation (mask spec shape, where zero-vs-merge lives, interaction with width). HIGHEST leverage of task-170.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
