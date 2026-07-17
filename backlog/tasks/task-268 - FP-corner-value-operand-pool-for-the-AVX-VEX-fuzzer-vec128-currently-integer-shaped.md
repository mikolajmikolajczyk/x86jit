---
id: TASK-268
title: >-
  FP corner-value operand pool for the AVX/VEX fuzzer (vec128 currently
  integer-shaped)
status: In Progress
assignee: []
created_date: '2026-07-17 19:13'
updated_date: '2026-07-17 20:07'
labels:
  - fuzz
  - simd
dependencies:
  - TASK-267
ordinal: 298000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The fuzzer's operand generator Rng::vec128 in x86jit-tests/src/fuzz.rs draws from an integer-shaped table (0, u128::MAX, per-16-bit sign bits, ascending bytes, 0x7fff lanes, 0x00ff lanes) or fully random bits. It contains NO float corner values. The one real float bug the 137k-program run found (TASK-265, vcvtps2ph directed rounding) needed a directed rounding mode AND an operand small enough to underflow to coincide — under uniform random bits that is luck, which is why it took hours and a single seed. The float ops under test (convert, fma, float-horizontal, dpps, round) have their sharp edges exactly at FP special values that the current pool almost never produces.

Add an FP-aware operand mode: pack lanes from a corner set covering, per element width (f32 and f16, and f64 for the pd ops):
  +0, -0, +inf, -inf, qNaN, sNaN, smallest subnormal, largest subnormal, smallest normal, largest normal (f16 overflow boundary >65504 for cvtps2ph), 1.0, -1.0, values straddling a rounding boundary (x.5 ulp), and denormal-flush candidates.
Mix corner lanes with random lanes so both all-corner and corner-in-noise vectors occur. Keep an integer-heavy mode too (the integer/lane ops still need adversarial byte patterns) and pick per-program based on which op families the program contains, or just union the pools.

This makes the TASK-265 class of bug fall out in minutes of targeted fuzzing instead of hours, and hardens the other float ops (dpps NaN handling, fma, round) against the same blind spot.

Depends on TASK-267 (the op table lets the generator know a program is float-heavy and bias operands accordingly) though it can also land independently as a pure vec128 extension.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Rng gains an FP-corner operand generator covering signed zero/inf, quiet+signalling NaN, subnormal boundaries, smallest/largest normal, f16 overflow boundary, and half-ulp rounding straddles, for f16/f32/f64 lane widths
- [ ] #2 Programs containing float VEX ops draw operands from a pool that mixes corner lanes with random lanes (both all-corner and corner-in-noise vectors occur)
- [ ] #3 Integer/lane ops retain adversarial byte-pattern operands (no regression in their coverage)
- [ ] #4 A targeted run (cargo xfuzz --ops vcvtps2ph, once TASK-265 is fixed) exercises the underflow/overflow/subnormal boundaries within seconds, demonstrated by the coverage table
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
