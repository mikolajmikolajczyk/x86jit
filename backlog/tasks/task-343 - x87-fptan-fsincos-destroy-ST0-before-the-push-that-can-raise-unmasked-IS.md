---
id: TASK-343
title: 'x87: fptan/fsincos destroy ST(0) before the push that can raise unmasked #IS'
status: To Do
assignee: []
created_date: '2026-08-15 13:25'
labels:
  - m6-x87
dependencies: []
priority: medium
ordinal: 379000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
set_st(cpu, 0, if ext { x.tan_ext() } else { x.tan() });
    push(cpu, F80::from_i64(1));      // may raise unmasked #IS and abandon

Probe: unmask IE, fill all eight registers, fptan, observe through fxsave (not a waiting instruction):

    engine:   sw=0x82c1 TOP=0 ST(0) sig=0xc75922e5f71d3000 exp=0x3fff   (= tan(1.0))
    hardware: sw=0x82c1 TOP=0 ST(0) sig=0x8000000000000000 exp=0x3fff   (= 1.0, untouched)

The status words are BIT-IDENTICAL (B|ES|C1|SF|IE, the #IS-overflow signature); the only difference is that the engine destroyed the source operand.

SDM Vol 1 §8.5.1.1: 'If the invalid-operation exception is not masked, a software exception handler is invoked ... and the top-of-stack pointer (TOP) and source operands remain unchanged.'

Guest sees corruption: the #MF handler is entitled to find ST(0) as it was and instead finds a partially-computed result, so it cannot recover or re-execute. TOP is correct, which makes it harder to spot.

Sites: 2 — Fptan and Fsincos, the only arms that set_st before a push. All 30 operand! readers and 9 pop sites were traced; every other arithmetic arm calls raise before its first write.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Both compute into temporaries and commit only after the push has succeeded
- [ ] #2 A host-witnessed test compares ST(0) after an unmasked stack overflow on fptan
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
