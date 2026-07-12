---
id: TASK-235
title: >-
  perf: game-shaped microbench suite (SIMD kernels, dispatch stress, hotloop
  sweep)
status: To Do
assignee: []
created_date: '2026-07-12 20:21'
labels:
  - 'crate:bench'
  - 'goal:perf'
milestone: ps4-perf
dependencies: []
ordinal: 264000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tier-0 measurement foundation for the PS4/games JIT-perf track (doc-33). The current x86jit-bench workloads (sha256/fib) are dispatch-heavy tiny-block shapes, not game-representative. Add game-shaped workloads: SIMD float kernels (mat4 mul, vec4 transform, dot/FMA chains, SoA particle update), memcpy/memset bandwidth, tight integer hot loop, indirect-call-heavy (vtable dispatch). Deterministic, seconds-long, checked-in ELFs like hello_musl.elf. These become the perf harness that lets JIT-perf work proceed WITHOUT a running game (validated for correctness by the unicorn oracle, for speed by native ratio). Pairs with task-147 (bench v2 native ratios / compile-run split).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 x86jit-bench records native/interp/jit ratios for >=4 game-shaped workloads (SIMD-float, memcpy, hotloop, indirect-call); numbers deterministic + CI-gateable
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
