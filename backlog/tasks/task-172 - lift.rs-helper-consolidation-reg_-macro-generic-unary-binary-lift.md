---
id: TASK-172
title: 'lift.rs helper consolidation: reg_* macro + generic unary/binary lift'
status: To Do
assignee: []
created_date: '2026-07-08 20:29'
updated_date: '2026-07-08 20:40'
labels:
  - 'crate:core'
  - 'goal:refactor'
  - seq-2
dependencies: []
ordinal: 196000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Lift path (NOT hot — safe to abstract). (1) reg_xmm/reg_ymm/reg_zmm/reg_kmask are 4 near-identical 8-line extractors -> one macro/generic over the iced predicate+base. (2) ~12 short lift_* unary/binary helpers (lift_neg/lift_not/lift_bswap/etc.) follow 'lower -> emit one IrOp -> emit_write'; a generic lift_unary(mk_op)/lift_binary collapses them. ~220 lines. Explicitly EXCLUDES the interp.rs ALU/shift/rotate dispatch arms — those stay explicit (hot dispatch loop; closure/generic dispatch risks inlining/perf).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 reg_* unified via macro/generic; generic unary/binary lift helper adopted by the short lift_* fns; suite green
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
