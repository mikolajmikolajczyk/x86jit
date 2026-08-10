---
id: TASK-321
title: 'native oracle: capture MXCSR, full x87 state and ZMM16-31'
status: To Do
assignee: []
created_date: '2026-08-10 19:29'
labels: []
dependencies: []
priority: medium
ordinal: 357000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-313 closed the publication problem by narrowing the README. The capture is still narrow: native.rs substitutes defaults for x87 state, the snapshot model has no MXCSR, and vector capture stops at register 15.

So rounding-mode changes, FP exception flags, x87 status/tag corruption and EVEX writes to the high registers are never compared against hardware. Capture them from the signal frame and widen CpuSnapshot, then restore the unqualified three-way claim in the README.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 MXCSR, x87 status/tag/control and ZMM16-31 are captured and compared
- [ ] #2 A test dirtying each newly-captured field would have passed before and fails now
- [ ] #3 The README caveat added by task-313 is removed
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
