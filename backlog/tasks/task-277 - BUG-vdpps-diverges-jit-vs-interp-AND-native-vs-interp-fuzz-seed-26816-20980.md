---
id: TASK-277
title: >-
  BUG: vdpps diverges jit-vs-interp AND native-vs-interp (fuzz seed 26816 /
  20980)
status: To Do
assignee: []
created_date: '2026-07-22 06:17'
labels:
  - bug
  - simd
  - avx
  - fuzz
dependencies: []
ordinal: 307000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The AVX/VEX differential fuzzer reports `vdpps` divergences on two axes:

- [JIT-vs-interp] ops=vpsignd,vdpps — seed 26816. This violates the HARD jit == interp invariant.
- [native-vs-interp] ops=vdpps — seed 20980, and again inside seed 26816.

Seed 26816 result (both tiers, reproducible):
    xmm3: expected 0x00000001ffffffff0000000000000001  got 0x801000007ff000000000000080100000
    xmm6: expected 0xffffffffffffffff00000000ffffffff  got 0x7ff000007ff00000000000007ff00000

The 'got' lanes are 0x7ff00000 / 0x80100000 — +inf and a small negative denormal in f32 — where the expected lanes are all-ones / small integers. That shape says the divergence is in how the dot-product accumulates and in NaN/inf lane handling, not a wholesale wrong opcode.

PRE-EXISTING, not caused by task-276: verified by stashing the task-276 opt_level change entirely and re-running seed 26816 on the pre-change tree (Cranelift default opt_level=none) — byte-identical divergence. Recorded here so the next person does not re-derive it.

Distinct from TASK-272, which covers the vfmadd/vfmaddsub/vfmsubadd family. vdpps is a different lowering (DPPS imm8 blend/dot, lifted in TASK-256/263).

Reproduce: cargo run --release -p x86jit-tests --bin fuzz -- --seed 26816
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The jit-vs-interp divergence on seed 26816 is root-caused and fixed — jit == interp holds for vdpps
- [ ] #2 The native-vs-interp vdpps divergence is either fixed or explicitly justified as an unspecified-result case with evidence from the SDM
- [ ] #3 A regression test covers vdpps against the native oracle for the inf/NaN/denormal lanes in the repro
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
