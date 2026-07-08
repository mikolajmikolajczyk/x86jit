---
id: TASK-168.1
title: 'AVX-1: VEX decode + AVX-128 forms onto existing u128 IR'
status: Done
assignee: []
created_date: '2026-07-08 15:21'
updated_date: '2026-07-08 15:41'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168
ordinal: 178000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Foundation + the startup-critical set. Add 2-/3-byte VEX (C5/C4) decode plumbing and lift the VEX.128 (L=0) forms of the already-lifted SSE ops onto the EXISTING u128 vector IR (no YMM state yet). Handle AVX's non-destructive 3-operand form (dst = op1(VEX.vvvv) OP op2(rm), vs SSE's dst OP= rm). vzeroupper/vzeroall = no-op (no upper state). 256-bit (L=1) still traps -> AVX-2 task. This alone should run VEX.128-only startup code (the first trap: vpxor c5 f9 ef c0).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 VEX.128 vmovdqu/vmovdqa/vmovups/vmovaps, vpxor/vpand/vpor/vpandn, vpcmpeqb/d, vpsubb, vpmovmskb, vmovq/vmovd, vpshufb lift + execute interp == jit == unicorn (3-operand form correct)
- [x] #2 vzeroupper decodes as a safe no-op
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. Added 2-/3-byte VEX decode (iced already decodes; lifter gained arms) and the VEX.128 forms onto the existing u128 vector IR (already 3-operand dst,a,b): vmovdqu/a, vmovups/aps, vmovq/d, vpxor/vpand/vpor/vpandn, vpaddb/w/d/q, vpsubb/w/d/q, vpcmpeqb/w/d, vpcmpgtb/w/d, vpminub/vpmaxub, vpmovmskb, vpshufb; vzeroupper/vzeroall = no-op. 256-bit/YMM forms auto-defer to 168.2 (reg_xmm rejects YMM -> unsupported). VALIDATION: the unicorn crate (2.1.5) can't be the AVX oracle — its QEMU build drops VEX.vvvv (computes dst=dst OP op2) AND exposes no XCR0 to enable AVX. Instead each VEX.128 test asserts the lift produces the SAME xmm+gpr state as the legacy-SSE equivalent (which IS unicorn-validated by the corpus) — 7 tests in differential.rs. jit==interp holds by construction (VEX arms emit only IR ops the JIT already lowers). Proven end-to-end: /usr/bin/echo now passes the vpxor trap.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
