---
id: TASK-249
title: >-
  lift AVX/SSE2 vpsadbw/psadbw — VEX.128 + legacy 66.0F F6 packed
  sum-of-abs-diff of bytes
status: To Do
assignee: []
created_date: '2026-07-15 11:38'
labels: []
dependencies: []
ordinal: 279000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Lift psadbw (66 0F F6 /r) and VEX.128 vpsadbw (VEX.128.66.0F.WIG F6 /r), which currently hard-fault the JIT with UnknownInstruction. PSADBW computes, per 64-bit half, sum over the eight bytes of abs(unsigned a.byte[i] - b.byte[i]), stored in the low 16 bits of that half with bits 63:16 zeroed. VEX.128 form clears bits 255:128 of the destination. Same operand shape as the SSSE3 horizontal ph* ops (task-247), so it rides the existing VHInt/VHIntM IR + shared hint helper via a new HIntOp::Sad variant, implemented in both the interpreter and the Cranelift JIT tiers. Differential coverage against the Unicorn oracle for the legacy form; VEX.128 validated against the SSE lowering (vex_eq_sse), incl. edge cases (max byte diff 0x00 vs 0xFF -> 2040, identical -> 0, mixed, upper-bits zeroing).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 psadbw (66 0F F6) lifts and executes in both interpreter and Cranelift JIT tiers
- [ ] #2 VEX.128 vpsadbw lifts in both tiers and zeroes bits 255:128 of the destination
- [ ] #3 differential/Unicorn coverage added for legacy + VEX.128 forms with edge cases; full suite + differential harness green
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
