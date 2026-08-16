---
id: TASK-340
title: >-
  x87: store and encode paths ignore rounding control and never report
  inexactness
status: To Do
assignee: []
created_date: '2026-08-15 13:25'
labels:
  - m6-x87
dependencies: []
priority: high
ordinal: 376000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Two related defects, both host-witnessed.

A. f80.rs:366-373 — a masked-underflow denormal result is TRUNCATED, not rounded. `to_bytes` denormalizes with a bare `self.sig >> shift`. `finish_exc` has already computed `denormalization_loses` and set UE|PE, but nothing rounds.

    fld tbyte(2^-16382 * (1+2^-63)); fdiv qword[4.0]; fstp tbyte
    engine   RC=near sig=0x2000000000000000    RC=up sig=0x2000000000000000   <-- unchanged
    hardware RC=near sig=0x2000000000000000    RC=up sig=0x2000000000000001

SDM Vol 1 Table 4-9: round up is 'closest to but no less than the infinitely precise result'. Truncation gives strictly less. This is also a double-rounding site: rounded once to 64 bits at unbounded exponent, then chopped onto the denormal grid.

B. x87.rs:837,846 — `fst`/`fstp` m64/m32 ignore RC, never set PE or C1, and the m32 arm double-rounds through f64. `F80::to_f64` is hardcoded round-to-nearest and returns no Exc.

    fstp qword(1+2^-63)  engine RC=near 0x3ff0000000000000 sw=0x0000
                         engine RC=up   0x3ff0000000000000 sw=0x0000
                       hardware RC=near 0x3ff0000000000000 sw=0x0020 (PE)
                       hardware RC=up   0x3ff0000000000001 sw=0x0220 (PE, C1=1 rounded up)
    fstp dword(1+2^-24+2^-60)  engine 0x3f800000   hardware 0x3f800001

SDM Vol 1 Table 4-9 (RC governs every rounded result), §4.8.4 and §4.9.1.6 (PE on an inexact result), §8.1.3.2 (C1 as the rounded-up indicator). The m32 case is textbook double rounding: 2^-60 is discarded by the intermediate f64 rounding, turning a value above the f32 tie into an exact tie that then goes to even — and it happens in the DEFAULT rounding mode, so it is the common path.

Sites: 1 for A (F80::to_bytes, the only encode path — reached from set_st and push, i.e. every arithmetic result written back); 2 for B (FstpF64|FstF64|FstpF32|FstF32).

Guest also sees fetestexcept(FE_INEXACT) read clean after an inexact long-double store.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 to_bytes rounds the denormal result under the current RC instead of truncating
- [ ] #2 fst/fstp m32/m64 round under RC in ONE step (no 80->64->32), set PE when inexact, and set C1 when they rounded up
- [ ] #3 Host-witnessed tests cover all four rounding modes for both, including the f32 double-rounding case in round-to-nearest
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
