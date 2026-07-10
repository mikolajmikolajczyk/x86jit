---
id: TASK-101
title: 'M8-T4 — MXCSR / vector FP flags (rounding-mode control, exception flags). No p'
status: To Do
assignee: []
created_date: '2026-07-06 11:07'
updated_date: '2026-07-10 16:25'
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
vldmxcsr/vstmxcsr VEX aliases landed in the FMA commit (43b90fc). Investigation 2026-07-10: MXCSR rounding-mode modeling is NOT the python numeric blocker — math.sqrt/exp/sin are bit-correct with the no-op ldmxcsr (default round-to-nearest suffices for libm here). The real python numeric bug is int->double conversion (task-202), orthogonal to MXCSR. Full RC/exception-flag modeling remains demand-driven — only needed if a guest depends on fesetround().
<!-- SECTION:NOTES:END -->
