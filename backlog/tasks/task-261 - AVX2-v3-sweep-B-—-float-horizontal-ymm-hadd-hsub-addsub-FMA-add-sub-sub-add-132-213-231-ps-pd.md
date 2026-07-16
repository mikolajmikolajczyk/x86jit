---
id: TASK-261
title: >-
  AVX2 v3 sweep B — float horizontal ymm (hadd/hsub/addsub) + FMA
  add-sub/sub-add (132/213/231 ps/pd)
status: Done
assignee: []
created_date: '2026-07-16 14:11'
updated_date: '2026-07-16 15:41'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 291000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Widen Vhaddpd/ps, Vhsubpd/ps, Vaddsubpd/ps to ymm (256-bit two-128-half). Add FMA alternating-sign family: Vfmaddsub132/213/231 ps/pd and Vfmsubadd132/213/231 ps/pd (xmm+ymm) reusing the existing VFma machinery with an alternating per-lane sign. All three tiers, jit==interp, native-oracle + jit tests per task-259 pattern. Owns: FMA IrOp/enum, float-horizontal + addsub helpers.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 All listed forms lift 3 tiers; jit==interp + native oracle green
- [ ] #2 clippy -D + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
