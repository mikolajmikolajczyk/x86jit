---
id: TASK-326
title: 'AVX float divergences from hardware: FMA and vdpps'
status: To Do
assignee: []
created_date: '2026-08-11 11:03'
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

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
