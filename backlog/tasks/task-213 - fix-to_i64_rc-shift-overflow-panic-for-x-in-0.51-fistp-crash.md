---
id: TASK-213
title: 'fix to_i64_rc shift-overflow panic for |x| in [0.5,1) (fistp crash)'
status: Done
assignee: []
created_date: '2026-07-11 11:38'
updated_date: '2026-07-11 11:53'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-11. Fixed to_i64_rc shift-overflow panic for |x| in [0.5,1). Root: guard was 'if e < -1' (fraction path for <0.5), so e==-1 ([0.5,1)) fell to the integer-shift path with shift=63-(-1)=64 -> '1u64<<64' panic. fistp/fist/fisttp of e.g. 0.75 crashed. Fix: added e==-1 branch using decide_round(0, sig, 2^63, sign, rc) — int part 0, whole sig is fraction, half-point 2^63; correct for all 4 rounding modes. Tests: to_i64_rc_handles_half_to_one_range (unit, 4 modes + boundaries) + x87_fistp_subunit_range_matches_unicorn (differential, [0.5,1) + negative x 4 rounding modes, bit-exact vs real FPU). 21 x87/f80 tests green, clippy+fmt clean.
<!-- SECTION:NOTES:END -->
