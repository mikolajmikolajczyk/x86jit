---
id: TASK-342
title: >-
  x87: fprem never writes C2 and returns the unreduced dividend for large
  quotients
status: To Do
assignee: []
created_date: '2026-08-15 13:25'
labels:
  - m6-x87
dependencies: []
priority: high
ordinal: 378000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`F80::rem` does `to_i64_rc(3)` on the quotient, which saturates to i64::MIN once |a/b| >= 2^63, and the `Fprem` arm never calls `set_codes`.

    engine:   after ONE fprem  st0 sig=0xc9f2c9cd1c675000 exp=0x4062  sw=0x3000  C2=0
              (1e30 is sig 0xc9f2c9cd04674ede exp 0x4062 — essentially the unreduced dividend)
    hardware: after ONE fprem  st0 = 5076964154930102272  sw=0x3400  C2=1
              after the architectural loop (1: fprem / fnstsw %ax / sahf / jp 1b)
              -> st0 = 1 = fmod(1e30,3)

SDM Vol 1 Table 8-10 and the Vol 2A FPREM entry make C2 the incomplete-reduction indicator. The engine reports 'reduction complete' while handing back a value ~3e29 times larger than the divisor.

Two distinct guest failures:
- WRONG RESULT from fmodl/fmod/remainder whenever the quotient is >= 2^63: the reduction loop exits on its first iteration and takes the dividend.
- HANG: because C2 is never WRITTEN, a C2 left set by an earlier ficom on a NaN (x87.rs:1142, the only other set_codes caller) makes that same loop spin forever.

Fprem is also the only arithmetic arm that discards the Exc return entirely — F80::rem has no _ctl form, so fprem ignores rounding and precision control too.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 fprem performs the architectural partial reduction, setting C2=1 when incomplete and C0/C3/C1 with the quotient bits, and C2=0 when complete
- [ ] #2 A test runs the architectural fprem loop to convergence for a quotient above 2^63
- [ ] #3 A test proves a stale C2 from a previous instruction cannot make the loop spin
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
