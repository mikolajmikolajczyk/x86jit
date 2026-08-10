---
id: TASK-315
title: fldenv/fnstenv does not round-trip the x87 environment
status: To Do
assignee: []
created_date: '2026-08-10 15:39'
labels: []
dependencies: []
priority: medium
ordinal: 351000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
x87.rs (~423): load_env28 keeps the control word and TOP and discards exception flags, condition codes, the tag word, FIP/FDP, the selectors and the opcode; env28 then regenerates zeros or derives the tag from live register values.

So fldenv followed by fnstenv returns a different image than it was given, and a pending exception restored by fldenv can never affect later execution. FreeBSD's fenv.h round-trip around powf/expf — the sequence that made this instruction necessary in the first place — is exactly this pattern.

TASK-237 covers the tag word's inability to express empty. This is the wider case: the rest of the environment is not modelled at all.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An arbitrary 28-byte environment image survives fldenv followed by fnstenv unchanged
- [ ] #2 Restored exception flags affect subsequent execution
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
