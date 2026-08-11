---
id: TASK-82
title: 'M8-T4 — MXCSR / vector FP flags (rounding-mode control, exception flags). No p'
status: Done
assignee: []
created_date: '2026-07-06 11:07'
updated_date: '2026-08-11 11:01'
labels:
  - 'crate:core'
  - 'crate:cranelift'
  - 'goal:feature'
milestone: open-backlog
dependencies: []
ordinal: 101000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
MXCSR / vector FP flags (rounding-mode control, exception flags). No program has demanded it; convert-to-int saturates (x86 integer-indefinite deferred). (testing.md §10)
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 differential FP test: rounding-mode changes via ldmxcsr observably alter cvt/add results jit==interp
- [ ] #2 exception-flag sticky bits (stmxcsr readback) compared vs oracle
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Closed into backlog/docs/deferred.md 2026-08-11 — the content is a decision not to build this, and deferred.md is where that belongs. Carrying it as an open task made the board claim work that nobody intends to start, and duplicated the document whose whole job is to say 'do not add this unprompted'.
<!-- SECTION:NOTES:END -->
