---
id: TASK-326
title: 'AVX float divergences from hardware: FMA and vdpps'
status: To Do
assignee: []
created_date: '2026-08-11 11:03'
updated_date: '2026-08-11 21:15'
labels: []
dependencies: []
priority: medium
ordinal: 362000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Both surfaced from the same fuzz campaign once task-205 made NaN-payload tolerance safe, and both are native-vs-interp — so the softfloat interpreter, the JIT's oracle, is itself wrong.

**FMA** (vfmaddsub/vfmsubadd/vfmadd): a subnormal f32 result divergence (0x05f8 vs 0x0678 — double rounding or an unfused a*b+c), a large finite sign divergence on vfmsubadd213ps, and a ±inf lane flip on the pd forms. Not NaN noise; real arithmetic.

**vdpps** diverges on two axes at once — jit-vs-interp (seed 26816, which violates the hard invariant) and native-vs-interp (seeds 20980 and 26816).

Kept together because they are one investigation: both are dot-product/fused-multiply paths through the same float helpers, both were found by the same tool, and the vdpps jit-vs-interp axis is the loose thread most likely to explain the rest. Merged from task-206 and task-211.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 vdpps agrees jit-vs-interp — the hard invariant is restored first
- [ ] #2 FMA subnormal, inf-sign and NaN-quieting behaviour matches hardware, with the seeds above as witnesses
- [ ] #3 Whether one root cause explains both is recorded either way
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Third witness added 2026-08-11 (found while closing TASK-325, unrelated to that task's changes): SEED 5915 is a jit != interp violation of the hard invariant on ROUNDPD, and the interpreter is the wrong side. The interpreter leaves a signalling NaN as it found it (0x7ff0000000000001); the JIT and the real CPU both quiet it to 0x7ff8000000000001. Reproduce with 'cargo xfuzz --seed 5915' — the shrinker reduces the program to a single VNew{op:1} (roundpd), and it reproduces identically with and without the new --mem leg, so it is pre-existing.

Worth taking BEFORE the FMA work: it is one instruction, one operand, and it is the same question the FMA items raise — where the softfloat helpers decide to quiet a NaN — on the smallest possible case. AC#2 already names NaN quieting; this is a concrete witness for it.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
