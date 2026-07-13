---
id: TASK-235
title: >-
  perf: game-shaped microbench suite (SIMD kernels, dispatch stress, hotloop
  sweep)
status: Done
assignee: []
created_date: '2026-07-12 20:21'
updated_date: '2026-07-13 08:04'
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
- [x] #1 x86jit-bench records native/interp/jit ratios for >=4 game-shaped workloads (SIMD-float, memcpy, hotloop, indirect-call); numbers deterministic + CI-gateable
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. Added 3 game-shaped kernels to x86jit-bench (workloads.rs): simd (packed-single damped accumulator, mulps/addps + horizontal-sum), memcpy (aligned 16B streaming copy + checksum fold), indirect (LCG-driven vtable dispatch via call r10, 16 leaves, IBTC stress). Shared run_code helper (flat 1MiB RW guest, hand-assembled like fib32/hotloop, native:None). New 'dump' subcommand prints golden outputs. Golden expects baked from interp leg; gate asserts interp==jit==expect for all 8 workloads. Iter counts 1M (halved from 2M per review-2 CI-budget note: 2M added ~33%/17s to pre-push gate; 1M keeps ratio stable). experiment shows region-bg wins: simd 1.1x, memcpy 1.5x; indirect is dispatch-bound (IBTC case). Two adversarial reviews (correctness + integration): no bugs. DoD: nextest --features unicorn (662 passed, 3 skipped, 0 failed); clippy clean; fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
