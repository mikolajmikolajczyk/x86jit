---
id: TASK-335
title: 'interp: VEX.256 writes leave bits 511:256 of the ZMM destination stale'
status: To Do
assignee: []
created_date: '2026-08-15 13:24'
labels:
  - m8-simd
dependencies: []
priority: high
ordinal: 371000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
SDM Vol 2A, VADDPD, Description: 'VEX.256 encoded version: ... The upper bits (MAXVL-1:256) of the corresponding ZMM register destination are zeroed.'

Both tiers leave them standing, so `jit == interp` is structurally blind to it. Confirmed end-to-end with no snapshot poking — `vmovdqu64 zmm0,[m]` then `vpxor ymm0,ymm0,ymm0` then `vmovdqu64 [m],zmm0`:

    interp zmm0 after vpxor ymm0: [00 x32, ab x32]     <- 511:256 stale
    jit    zmm0 after vpxor ymm0: [00 x32, ab x32]     <- identical

Per-op sweep on the interpreter: vaddps, vpaddd, vpxor, vpslldq, vblendps, vpblendw, vperm2i128, vpermq, vpshufb, vpsrld, vsqrtps, vshufps, vinsertf128 all STALE. vpshufd ymm and vmovdqu ymm are CORRECT because they route through `set_vec`, which zeroes above `bytes`.

`vpxor ymm, ymm, ymm` is the canonical compiler-emitted zeroing idiom in AVX-512 code, so a mixed VEX.256/EVEX.512 sequence is ordinary output, not a corner case.

Blast radius: 39 functions in interp/vector.rs assign `cpu.ymm_hi[dst]` directly without touching `zmm_hi`, plus 10 `set_vec_low` call sites — `set_vec_low` (state.rs:561) preserves everything above `bytes` by design.

lift/vector.rs:181-183 asserts the opposite as fact: '256-bit results legitimately fill 255:128 (their >256 zeroing is handled by the width-aware write)'. On these paths the width-aware write IS `set_vec_low`, which does not zero. That comment needs correcting with the fix.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A VEX.256 write zeroes 511:256 on both tiers, pinned by a test that dirties zmm_hi first
- [ ] #2 The fix is in one place (the width-aware write or the lift), not repeated across 39 handlers
- [ ] #3 The false claim at lift/vector.rs:181-183 is corrected
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
