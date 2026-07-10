---
id: TASK-197.5
title: 'MODE-A.5: 32-bit differential + fuzz coverage'
status: To Do
assignee: []
created_date: '2026-07-10 10:32'
labels:
  - guest-modes
dependencies:
  - TASK-197.1
parent_task_id: TASK-197
ordinal: 226000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Wire the existing unicorn differential and fuzzer to run a Compat32 lane (unicorn UC_MODE_32). Reuse the 64-bit case tables where encodings are shared; add 32-bit-only cases (address wrap, 67h forms, 16-bit stack ops, inc/dec short forms 0x40-0x4F which are REX bytes in long mode). This is the safety net every other A subtask cites.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Differential harness runs a 32-bit lane vs unicorn UC_MODE_32
- [ ] #2 Fuzzer generates and diffs Compat32 blocks (incl. 0x40-0x4F inc/dec forms)
- [ ] #3 CI job (manual dispatch, per repo convention) covers the 32-bit lane
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
