---
id: TASK-188
title: >-
  Deepen the x87 differential: compare the x87 stack, inexact values,
  transcendentals
status: Done
assignee: []
created_date: '2026-07-09 12:51'
updated_date: '2026-07-10 22:12'
labels:
  - m8-simd
dependencies: []
ordinal: 212000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
x87 is true 80-bit (F80 software float, x86jit-core/src/f80.rs) but its tests are shallow: x87_matches_unicorn uses only exactly-representable values and does NOT compare the x87 register stack (reads results back into GPRs). Precision/rounding/stack bugs are structurally invisible. Deepen: compare the full ST(0..7) stack + status/control words against Unicorn, add inexact operands (rounding-sensitive), and cover transcendentals (fsin/fcos/fptan/f2xm1/fyl2x) + fbstp (BCD) which are currently untested. Also fix the stale module comment in x87.rs:1 (says f64-backed; it is F80-backed since f80.rs landed).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 differential compares the full x87 stack (depth + tags), not just stored results
- [x] #2 inexact-value cases (fdiv producing non-representable) compared bit-exact vs oracle
- [x] #3 transcendentals (fsin/fcos/fpatan/f2xm1) differential with documented ULP tolerance
- [x] #4 stale x87.rs:1 comment fixed
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#3 adapted: the interp does NOT lift x87 transcendentals (fsin/fcos/fptan/f2xm1/fyl2x/fpatan all trap UnknownInstruction), so an interp-vs-Unicorn differential of OUR impl is impossible. Delivered instead: a tripwire test asserting the trap (fails loudly when someone implements them, prompting a real differential) + a Unicorn-vs-libm ULP guard (4 ULP f64) exercising the new ST(0) capture. Filed the transcendental-lift gap as a follow-up. Stack/control/inexact ACs fully met + teeth-checked.
<!-- SECTION:NOTES:END -->
