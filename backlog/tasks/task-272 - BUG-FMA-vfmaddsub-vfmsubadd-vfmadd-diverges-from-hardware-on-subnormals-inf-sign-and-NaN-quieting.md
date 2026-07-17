---
id: TASK-272
title: >-
  BUG: FMA (vfmaddsub/vfmsubadd/vfmadd) diverges from hardware on subnormals,
  inf-sign, and NaN-quieting
status: To Do
assignee: []
created_date: '2026-07-17 21:14'
labels:
  - bug
  - simd
  - fuzz
dependencies:
  - TASK-271
ordinal: 302000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Surfaced by the AVX fuzz campaign once TASK-271 made the NaN-payload tolerance SAFE (width-restricted + quiet/signaling veto). With genuine unspecified-NaN-payload noise correctly filtered, the residual --family convert,fma divergences are mostly REAL FMA correctness bugs, native-vs-interp AND jit-vs-interp (so both the softfloat interp and the JIT diverge from the real CPU — the interp/JIT FMA is not bit-accurate).

Evidence (cargo xfuzz --family convert,fma):
- vfmaddsub213ps, single op: xmm4 expected 0x…05f8…05f8 got 0x…0678…0678. Both are SUBNORMAL f32 (exp 0), not NaN — a real subnormal FMA result divergence (double-rounding / unfused a*b+c, or subnormal handling).
- vfmsubadd213ps, single op: xmm0 expected 0x7f01807f… got 0xc001807f… — a large finite divergence (positive vs negative), suggesting the add/sub lane pattern or an intermediate sign differs.
- vfmaddsub213pd/vfmsubadd213ps: ymm5.hi expected 0xfff0000000000000 (−inf f64) got 0x7ff0000000000000 (+inf), and the paired lane flips ±maxfinite — a sign/lane divergence, not NaN.
- qNaN-vs-sNaN class mismatches (e.g. lane 0x7ff0000000000001 sNaN vs 0x7ff8000000000001 qNaN): the interpreter appears to emit a SIGNALING NaN where hardware emits a QUIET NaN. Hardware never produces an sNaN result; if interp does, that is a real NaN-quieting bug (distinct from the tolerated payload/sign noise).

These are NOT the unspecified-NaN-sign/payload class (TASK-271 tolerates those and this task is what is LEFT after that filter). The likely root is that the interpreter computes FMA as a*b then +c with an intermediate rounding (unfused) and/or mishandles subnormal flush and NaN quieting, while the JIT uses the host FMA (fused) — so both differ from each other and from hardware in different ways.

Investigate: find the interp FMA implementation (grep exec for vfmadd/fma / the FMA helper shared with cranelift), compare against a true fused-multiply-add with correct subnormal + NaN-quieting semantics (softfloat f32/f64 fma), and make interp == JIT == hardware. Each sub-divergence (subnormal, inf-sign/lane, NaN-quieting) may need its own fix + native regression test proven red-without-fix.

Reproduce: cargo run --release -p x86jit-tests --bin fuzz -- --family fma --secs 30   (multiple distinct signatures; shrink each with --seed N).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The interpreter FMA (vfmadd/vfmaddsub/vfmsubadd, ps/pd) is a correctly-rounded fused multiply-add: single-rounding, correct subnormal handling, and produces a QUIET NaN (never signaling), matching the NativeOracle
- [ ] #2 jit == interp for all FMA forms across finite/subnormal/inf/NaN inputs
- [ ] #3 Native-vs-interp regression tests cover the subnormal, inf-sign/add-sub-lane, and NaN-quieting cases and are proven to FAIL without the fix
- [ ] #4 cargo xfuzz --family fma --secs 120 reports zero non-NaN-payload divergences (the TASK-271 tolerance still applies to genuine unspecified payload)
- [ ] #5 cargo nextest -E 'not binary(fuzz_robustness)' and clippy -D warnings pass
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
