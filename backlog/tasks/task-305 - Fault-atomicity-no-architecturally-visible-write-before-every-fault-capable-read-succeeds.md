---
id: TASK-305
title: >-
  Fault atomicity: no architecturally visible write before every fault-capable
  read succeeds
status: To Do
assignee: []
created_date: '2026-08-10 15:37'
labels: []
dependencies: []
priority: high
ordinal: 337000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A precise fault leaves the destination unchanged. Four places break that rule, in three subsystems, and an adversarial review found each independently without noticing they are one invariant — which is the argument for fixing them as one contract rather than four patches.

The rule to establish and then enforce: an instruction may not commit any guest-visible state until every read that can fault has succeeded. Compute into temporaries; commit last.

Subtasks carry the individual sites. This parent owns the invariant, where it is written down (conventions.md), and how it is tested — note that jit-vs-interp comparison cannot validate any of it, because both tiers share the shape. It needs native fault/retry witnesses.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The invariant is stated in conventions.md with the reason a differential test cannot check it
- [ ] #2 Every subtask closed
- [ ] #3 A native fault-and-retry witness exists for at least the 256-bit and the cross-page cases
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
