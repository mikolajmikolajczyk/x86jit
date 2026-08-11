---
id: TASK-314
title: x87 arithmetic ignores the control word's rounding and precision fields
status: Done
assignee: []
created_date: '2026-08-10 15:39'
updated_date: '2026-08-11 11:02'
labels: []
dependencies: []
priority: high
ordinal: 350000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
x87.rs (~451): the control word's RC is passed only to integer conversion. Every add/sub/mul/div path calls F80 operations fixed at nearest-even with a 64-bit significand, never consulting RC or PC.

So fldcw and fldenv visibly restore a control word and the arithmetic that follows behaves as though they had not. A guest that sets round-toward-zero, or 24/53-bit precision — which is what fldcw is for — gets silently wrong results, and no trap.

This is the third x87 finding of the same shape as TASK-237 (the tag word) and task-315 (the environment image): the control and status state is modelled as something to store and reload rather than something that governs execution.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 RC and PC reach every arithmetic operation and change its result
- [ ] #2 Halfway and inexact cases are tested under all four rounding modes and all three precisions, against hardware
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Merged into the x87/F80 fidelity task 2026-08-11. All four are the same gap — architectural state modelled as storage rather than as something that governs execution — and they share one hardware-comparison harness. Each kept its own acceptance criterion there.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
