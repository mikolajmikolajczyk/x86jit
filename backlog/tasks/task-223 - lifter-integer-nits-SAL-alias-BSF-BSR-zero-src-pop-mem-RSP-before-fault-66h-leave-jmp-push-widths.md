---
id: TASK-223
title: >-
  lifter/integer nits: SAL alias, BSF/BSR zero-src, pop [mem] RSP-before-fault,
  66h leave/jmp/push widths
status: Done
assignee: []
created_date: '2026-07-12 08:07'
updated_date: '2026-07-12 08:35'
labels:
  - 'crate:core'
  - bug
  - code-review
dependencies: []
ordinal: 252000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable whole-codebase review, LOW-severity correctness nits in the integer/lifter path: x86jit-core/src/lift/mod.rs, x86jit-core/src/interp/integer.rs, x86jit-cranelift/src/codegen/integer.rs. Do NOT touch elide_dead_flags (that is task-224, delicate, handled separately) — stay out of the flag-elision code. (1) SAL is the /6 encoding alias of SHL and is not lifted -> UnknownInstruction where hardware runs SHL. Add Sal alongside Shl in the lift match (lift/mod.rs ~516). (2) BSF/BSR with a zero source must leave the DESTINATION UNCHANGED (only ZF is set); current code zero-extends/writes the dest. Fix both interp and jit to preserve dest on zero source. (3) pop [mem] commits RSP before the store, so a faulting store leaves RSP already advanced — should compute the store, and on fault not have mutated RSP (match hardware: the pop is restartable). (4) 66h operand-size bugs: leave, jmp r/m16, push imm with a 66h prefix use the wrong width. Fix each to the 16-bit width. Confirm interp==jit for every fix and add coverage where cheap.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 SAL lifts (as SHL); no UnknownInstruction for the /6 alias
- [ ] #2 BSF/BSR leave dest unchanged on a zero source (ZF set), interp==jit
- [ ] #3 pop [mem] does not mutate RSP when the store faults
- [ ] #4 66h leave / jmp r/m16 / push imm use 16-bit width
- [ ] #5 cargo nextest (--features unicorn, minus fuzz_robustness) green; clippy -D warnings + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
