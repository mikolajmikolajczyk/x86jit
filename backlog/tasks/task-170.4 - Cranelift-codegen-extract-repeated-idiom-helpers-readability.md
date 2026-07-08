---
id: TASK-170.4
title: 'Cranelift codegen: extract repeated idiom helpers (readability)'
status: To Do
assignee: []
created_date: '2026-07-08 20:29'
updated_date: '2026-07-08 20:40'
labels:
  - 'crate:cranelift'
  - 'goal:refactor'
  - seq-2
dependencies: []
parent_task_id: TASK-170
ordinal: 195000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Mechanical helper extraction in codegen.rs (safe, no behavior change). (a) with_vec_cast(v, vty, f) wrapping the bitcast_v -> op -> bitcast_i128 sandwich (~91 sites); (b) zero_i128() for the iconst0+uextend pattern (~6); (c) call_helper_with_trap(...) for the flush/call/brif(exc,ok)/reload block-wiring repeated in X87/FxState/RepString/Div (4 sites, ~40 lines); (d) a register_helper! macro for the 7 near-identical Helpers registration blocks in lib.rs + the test mirror (I hit this adding xgetbv). ~110 lines + clutter removed. Child of 170 (shares the 256-arm template work in 170.2).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 with_vec_cast + zero_i128 + call_helper_with_trap helpers; register_helper! macro; behavior unchanged, suite green
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
