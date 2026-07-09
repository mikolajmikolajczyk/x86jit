---
id: TASK-168.5.6
title: 'AVX-512: EVEX lane ops vinserti32x4/64x2/64x4, valignd/q'
status: To Do
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 15:10'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 189000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
512-wide lane inserts (vinserti32x4/64x2/64x4 — 128/256-bit lane into ZMM) and cross-512 dword/qword align (valignd/q). Lower frequency (memcpy tails). Priority 6.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 jit_eq_interp(v4) differential snippet per lane op (vinserti32x4/64x2/64x4, valignd/q) across lane boundaries
- [ ] #2 compat map regenerated
<!-- AC:END -->
