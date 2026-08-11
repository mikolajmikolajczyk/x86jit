---
id: TASK-92
title: >-
  CR — efficiency: interp temps zero-fill / fresh_code_pages / trap_window
  recompute
status: Done
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-08-11 11:30'
labels:
  - 'crate:core'
  - 'goal:perf'
milestone: code-review
dependencies: []
ordinal: 129000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
interp.rs zero-fills the whole temps scratch per block dispatch (SSA define-before-use makes it unneeded); fresh_code_pages builds ~1M AtomicBool element-by-element at Reserved startup; vm.rs recomputes trap_window (full region scan) per block materialize.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 pure perf cleanup: existing suite green + bench regression gate is the coverage (no new tests required)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
MEASURED, THEN DECLINED. All three items are below noise; doing them would trade real risk for nothing, and this repository has already learned that lesson once (task-217: four micro-optimisations that measured well locally moved the real workload by zero).

Numbers, release profile, this host:
  - fresh_code_pages over the whole 4 GiB window: 35 us, ONCE per Vm construction. Not a hot path by any reading.
  - trap_window() with 65 mapped regions: 19 ns per call, called once per block MATERIALIZE — i.e. against a Cranelift compile measured in microseconds at best. Three orders of magnitude below its caller.
  - clear + resize(64, 0) on the interpreter temps: 5 ns per block dispatch, against a block interpretation of hundreds of nanoseconds upward.

The temps item is the one to be careful about even if it had been worth it: dropping the zero-fill rests on SSA define-before-use holding for every op, which nothing verifies. That is a correctness invariant traded for 5 ns.

If any of these ever matters it will be because a profile says so, and that belongs in the performance roadmap task, not here.
<!-- SECTION:NOTES:END -->
