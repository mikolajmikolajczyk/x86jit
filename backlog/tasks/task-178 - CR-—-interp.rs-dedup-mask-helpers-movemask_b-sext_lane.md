---
id: TASK-178
title: 'CR — interp.rs dedup: mask helpers, movemask_b, sext_lane'
status: Done
assignee: []
created_date: '2026-07-09 09:56'
updated_date: '2026-07-09 10:29'
labels:
  - CR
dependencies: []
ordinal: 202000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Code-quality consolidation in x86jit-core/src/interp.rs (the slow oracle). No behavior change. (E) low-bits-mask logic ((1<<bits)-1 saturating at width) duplicated as mask/lane_mask/kwidth_mask + ~5 inline copies -> route inline copies to the helpers. (F) VMoveMaskB vs VMoveMaskB256 copy-paste (256 = 128 loop twice) -> movemask_b helper. (G) sign-extend-in-place idiom (x^sign).wrapping_sub(sign) repeated 3x in packed_bin/packed_shift/vpcmp_mask -> sext_lane helper. Not perf-sensitive (vector/mask ops, not scalar ALU hot path). Verify: build + nextest + clippy.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 inline mask copies routed to mask/lane_mask/kwidth_mask,movemask_b helper kills 128/256 dup,sext_lane helper replaces the 3 copies,full suite green + clippy clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
