---
id: TASK-224
title: >-
  codegen: JIT VZeroUpper clears only ymm_hi, interp clears zmm_hi too — VEX.128
  write leaves guest bits 511:256 stale under AVX-512
status: Done
assignee: []
created_date: '2026-07-29 09:53'
updated_date: '2026-08-10 21:49'
labels:
  - bug
  - avx512
  - codegen
dependencies: []
ordinal: 320000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A VEX.128 register write must zero DEST[MAXVL-1:128]. With AVX-512 features enabled MAXVL is 512, so bits 511:128 must go to zero.

The interpreter does this: `exec_v_zero_upper` (x86jit-core/src/interp/vector.rs:2682) clears `ymm_hi` AND `zmm_hi`. The Cranelift backend does not: `store_ymm_hi_zero` (x86jit-cranelift/src/codegen/mod.rs:3593) emits two 8-byte stores to `ymm_hi` only and never touches `zmm_hi`. So every `IrOp::VZeroUpper` diverges between engines whenever the guest has a dirty zmm upper.

MEASURED at the then-current HEAD, GuestCpuFeatures::v4(), seeding zmm_hi[2] = [u128::MAX; 2] then running `vmovss xmm2,xmm4,xmm6` (c5 da 10 d6):

  interp: zmm_hi2 = 0, 0                                       <- correct
  jit:    zmm_hi2 = ffff..ffff, ffff..ffff                     <- bits 511:256 stale

Found while investigating task-223 (which turned out to be a false report); this is a real, separate defect in the same silent-because-usually-zero class. Invisible to the whole corpus because jit_eq_interp starts from an all-zero CpuSnapshot and no test seeds zmm_hi before a VEX.128 op.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 JIT VZeroUpper zeroes bits 511:128, matching the interpreter and the SDM
- [ ] #2 A jit_eq_interp test seeds zmm_hi (and ymm_hi) non-zero before a VEX.128 write and pins the whole 512-bit result
- [ ] #3 The legacy-SSE path still preserves the upper halves — no regression on set_vec_low semantics
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
FIXED 2026-08-10, and filed twice — see below. emit_v_zero_upper now clears both zmm_hi halves alongside ymm_hi; regression test vex128_write_clears_zmm_hi_of_its_destination in jit.rs, proven red against the old code. Landed in commit 43384e6.

This task was invisible on main's board (see the branch-id collision recorded in the tidy notes), so an adversarial review rediscovered the same bug and it was filed again as TASK-302. TASK-302 is the duplicate; this is the original.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
