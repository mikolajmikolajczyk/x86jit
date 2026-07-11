---
id: TASK-213
title: 'fix to_i64_rc shift-overflow panic for |x| in [0.5,1) (fistp crash)'
status: To Do
assignee: []
created_date: '2026-07-11 11:38'
labels:
  - 'crate:core'
  - 'goal:correctness'
dependencies: []
ordinal: 242000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found during task-212: F80::to_i64_rc (f80.rs ~259) panics 'attempt to shift right with overflow' for a Normal value with exp==-1 (|value| in [0.5,1)). The guard is 'if e < -1' (fraction path) so e==-1 falls to the integer-shift path with shift=63-(-1)=64 -> overflow. Real impact: fistp/fist/fisttp of e.g. 0.75 crashes the interpreter. task-212's reduce_quadrant avoided it with a bespoke round_to_i64; the general to_i64_rc still has the bug. Fix: handle e==-1 (round the pure fraction, like the e<-1 branch but rounding to 0 or ±1). Add a differential test: fistp of {0.4,0.5,0.6,0.75,0.9,-0.75} vs Unicorn across all 4 rounding modes.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
