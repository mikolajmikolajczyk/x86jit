---
id: TASK-170.3
title: 'SIMD: central register-file accessor (read_vec / write_vec_masked)'
status: To Do
assignee: []
created_date: '2026-07-08 20:24'
updated_date: '2026-07-08 20:40'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:refactor'
  - seq-1
dependencies: []
parent_task_id: TASK-170
ordinal: 193000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
One place to read/write N vector lanes over the scattered xmm[i]/ymm_hi[i]/zmm_hi[i][*] arrays, with optional k-mask + zero/merge. Enabler for both the masking abstraction and width-parameterization; today 512-bit code juggles 4 lane accessors inline.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
