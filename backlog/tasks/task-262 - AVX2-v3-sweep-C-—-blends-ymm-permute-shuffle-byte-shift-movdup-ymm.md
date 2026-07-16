---
id: TASK-262
title: AVX2 v3 sweep C — blends ymm + permute/shuffle/byte-shift/movdup ymm
status: Done
assignee: []
created_date: '2026-07-16 14:11'
updated_date: '2026-07-16 15:41'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 292000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Widen to ymm: Vblendpd/ps (imm8), Vblendvpd/ps, Vpblendvb, Vpblendw; Vpermilps/pd (reg + imm), Vpermps, Vpshufhw/lw, Vpslldq/Vpsrldq, Vmovddup/shdup/sldup. Parameterize the existing VBlend*/VShuffle*/VByteShift/VPermil IrOps by width (bytes 16 vs 32, two-128-half where lane-crossing rules require). All three tiers, jit==interp, native-oracle + jit tests per task-259. Owns those shuffle/blend helpers.
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
