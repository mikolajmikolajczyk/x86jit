---
id: TASK-92
title: >-
  INT-T5 — vDSO: expose a guest-visible vDSO or force
  `clock_gettime`/`gettimeofd
status: To Do
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-09 15:10'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: open-backlog
dependencies: []
ordinal: 92000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
vDSO: expose a guest-visible vDSO or force `clock_gettime`/`gettimeofday` down the syscall path. (Both are stubbed in the shim to a fixed epoch today.) (testing.md §12)
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 whole-program test: a glibc binary calling clock_gettime via vDSO path runs three ways (or the fallback forcing is asserted in a shim test)
<!-- AC:END -->
