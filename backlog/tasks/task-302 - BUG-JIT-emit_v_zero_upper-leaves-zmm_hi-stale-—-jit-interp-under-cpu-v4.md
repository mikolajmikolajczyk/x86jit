---
id: TASK-302
title: 'BUG: JIT emit_v_zero_upper leaves zmm_hi stale — jit != interp under --cpu v4'
status: Done
assignee: []
created_date: '2026-08-10 15:36'
updated_date: '2026-08-10 19:29'
labels: []
dependencies: []
priority: high
ordinal: 334000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
codegen/vector.rs:3253 emit_v_zero_upper clears only ymm_hi. The interpreter's exec_v_zero_upper (interp/vector.rs) clears ymm_hi AND zmm_hi[reg] = [0;2], and emit_v_zero_upper_all directly below the JIT version clears both zmm_hi halves with a comment saying it does so to match the interp oracle. So after any instruction dirties a register's ZMM upper half, a VEX.128 write lowered through VZeroUpper leaves bits 511:256 observable under the JIT and zeroed under the interpreter and on hardware.

TASK-164 ('vzeroupper leaves zmm_hi stale') is Done and covers this shape — either it fixed only the _all variant or this regressed since. Check which before writing the fix.

The differential suite misses it because reaching it needs a dirty zmm_hi BEFORE a VEX.128 write; nothing in the corpus sets that up. Found by an adversarial review, not by a test.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 emit_v_zero_upper clears both zmm_hi halves, matching the interpreter
- [ ] #2 A jit-vs-interp test seeds all upper halves nonzero, performs a VEX.128 write, and compares the full 512-bit destination
- [ ] #3 Whether TASK-164 regressed or was incomplete is recorded
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed: emit_v_zero_upper now clears both zmm_hi halves alongside ymm_hi, matching exec_v_zero_upper and emit_v_zero_upper_all. AC#3 answered: TASK-164 was INCOMPLETE, not regressed — its test drives the vzeroupper/vzeroall instructions (IrOp::VZeroUpperAll), never the single-register IrOp::VZeroUpper that a VEX.128 write emits as a trailing op. New test vex128_write_clears_zmm_hi_of_its_destination covers that path; negative control confirms it fails against the old code.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
