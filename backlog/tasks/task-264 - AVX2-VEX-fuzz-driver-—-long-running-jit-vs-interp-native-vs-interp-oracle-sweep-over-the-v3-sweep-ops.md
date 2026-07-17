---
id: TASK-264
title: >-
  AVX2 VEX fuzz driver — long-running jit-vs-interp + native-vs-interp oracle
  sweep over the v3 sweep ops
status: Done
assignee: []
created_date: '2026-07-17 14:15'
labels:
  - test
  - fuzz
  - simd
dependencies: []
ordinal: 294000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Delivered by commit 78bfbf9. Adds `x86jit-tests/tests/fuzz_avx.rs`, an `#[ignore]`d multi-hour driver that generates random programs containing at least one VEX/AVX2 op from the task-259..263 sweep and checks two oracles per program: JIT vs interpreter (a divergence is a codegen bug) and the real host CPU via NativeOracle vs interpreter (a divergence is a semantics bug — the ground truth for VEX, since Unicorn's QEMU mis-decodes VEX). Divergences are shrunk, deduplicated by (leg, op-signature) and appended to a log; the run never stops on the first failure, so one pass surfaces every distinct bug.

Also adds the `FuzzInsn::VVex` generator variant with a 63-op pool in `x86jit-tests/src/fuzz.rs`, gated behind a new `gen_avx()` entry point so the pre-existing fuzz tests (`native_matches_interp`, `unicorn_matches_interp`) keep their old instruction distribution.

The native leg skips any program containing a legacy-SSE vector op (`has_legacy_vec`): x86jit models those as clearing bits 255:128 while real hardware preserves the upper, a documented model choice that would otherwise flood the log with non-bugs.

First full run: 137166 programs, 22778 native-oracle runs, 25 distinct signatures, seed 1..453246. Triage found the integer and lane lowerings bit-exact vs hardware (zero genuine divergences); every real finding was float. Follow-ups filed from it: TASK-265 (f32_to_f16 directed rounding).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
