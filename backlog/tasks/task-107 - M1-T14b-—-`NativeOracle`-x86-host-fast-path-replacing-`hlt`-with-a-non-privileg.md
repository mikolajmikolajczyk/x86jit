---
id: TASK-107
title: >-
  M1-T14b — `NativeOracle` (x86-host fast path replacing `hlt` with a
  non-privileg
status: To Do
assignee: []
created_date: '2026-07-06 11:07'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:tests'
milestone: open-backlog
dependencies: []
ordinal: 107000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`NativeOracle` (x86-host fast path replacing `hlt` with a non-privileged terminator). Optional — Unicorn already provides the truth. (testing.md §2, §4)
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
