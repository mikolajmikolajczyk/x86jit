---
id: TASK-258
title: >-
  VEX.256 YMM float sweep — vcvtdq2ps/ps2dq/tps2dq,
  vadd/sub/mul/div/min/max/sqrt ps/pd, vshuf/vunpck ymm (Celeste blocker c5 fc
  5b c0)
status: Done
assignee: []
created_date: '2026-07-15 23:23'
updated_date: '2026-07-15 23:40'
labels: []
dependencies: []
ordinal: 288000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Celeste (Mono+FNA) faults on 256-bit YMM VEX float ops. Concrete blocker: c5 fc 5b c0 = vcvtdq2ps ymm0,ymm0. Extend the existing 128-bit VEX float lifts to the upper 128-bit YMM lane (ymm_hi), all three tiers (decode/interp/cranelift). Bitwise ymm already done via VLogic256; cmpps/pd ymm already done via VFloatCmpMask256.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vcvtdq2ps ymm (c5 fc 5b c0) decodes+runs, no fault
- [x] #2 vadd/sub/mul/div/min/max ps+pd ymm lifted, all 3 tiers
- [x] #3 vsqrtps/pd ymm lifted
- [x] #4 vcvtps2dq/vcvttps2dq ymm lifted
- [x] #5 vshufps/pd + vunpck{l,h}p{s,d} ymm lifted
- [x] #6 differential + native-oracle tests green; clippy+fmt clean; compat map regenerated
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
