---
id: TASK-263
title: >-
  AVX2 v3 sweep D — width-changing converts + movmsk/test/round/dpps ymm +
  horizontal-int/sign ymm + string specialists
status: To Do
assignee: []
created_date: '2026-07-16 14:11'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 293000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Converts: Vcvtdq2pd, Vcvtps2pd, Vcvtpd2ps, Vcvtpd2dq, Vcvttpd2dq, Vcvtph2ps, Vcvtps2ph. Misc ymm: Vmovmskps/pd, Vtestps/pd (xmm+ymm), Vroundpd/ps, Vdpps. Widen lift_vhint (Vphaddd/w/sw, Vphsubd/w/sw, Vpsadbw) and lift_vpsign (Vpsignb/w/d) to ymm. Specialists: Vmpsadbw, Vphminposuw, Vpcmpestri64/estrm64. All three tiers, jit==interp, native-oracle + jit tests per task-259. Owns lift_vhint/lift_vpsign widening. If a specialist proves genuinely large, note it and leave a scoped follow-up rather than half-implement.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Converts + misc + horizontal/sign ymm lift 3 tiers; jit==interp + native oracle green
- [ ] #2 clippy -D + fmt clean; any deferred specialist documented
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
