---
id: TASK-189
title: Lift mul r8 / imul r8 (8-bit one-operand multiply) — fuzzer found it unlifted
status: Done
assignee: []
created_date: '2026-07-09 13:14'
updated_date: '2026-07-10 21:27'
labels:
  - code-review
dependencies: []
ordinal: 213000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The extended differential fuzzer (task-185) found the 8-bit one-operand multiply mul r/m8 and imul r/m8 (F6 /4 and /5: AL*src8 -> AX) is NOT lifted — the interpreter returns Exit::UnknownInstruction while Unicorn/real hardware run it. The 16/32/64-bit forms (F7 /4,/5) are lifted. Add the 8-bit form to lift.rs (widening AL*src8 -> AX; CF/OF set from AH != 0), interp, and codegen; then re-enable size 1 for Mul1 in the fuzzer (size248 currently skips it). Low-frequency but a real ISA gap.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->



## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 jit_eq_interp differential snippet covers mul r8 and imul r8 (AH:AL result + flags)
- [x] #2 fuzzer Mul1 menu re-extended to size 1 (the restriction added in task-185 is lifted)
- [x] #3 compat map regenerated — mul/imul r8 PRESENT
<!-- AC:END -->
