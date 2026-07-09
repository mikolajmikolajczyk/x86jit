---
id: TASK-168.5.6
title: 'AVX-512: EVEX lane ops vinserti32x4/64x2/64x4, valignd/q'
status: Done
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 20:14'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 189000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
512-wide lane inserts (vinserti32x4/64x2/64x4 — 128/256-bit lane into ZMM) and cross-512 dword/qword align (valignd/q). Lower frequency (memcpy tails). Priority 6.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 jit_eq_interp(v4) differential snippet per lane op (vinserti32x4/64x2/64x4, valignd/q) across lane boundaries
- [x] #2 compat map regenerated
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed. vinserti32x4/64x2 (128-bit lane insert) + vinserti64x4/32x8 (256-bit insert) + their f-variants via lift_vinsert_wide → VInsertLaneWide (interp + jit inline via load_lane/store_lane, pre-reading ins to handle dst aliasing). valignd/q via lift_valign → VAlign (interp valign_lanes byte-shift of the a:b concatenation; jit via a new valign_helper → exec_valign, low-freq). Register src only, masking + memory src deferred. Tests: jit avx512_lane_ops_match_interp (all 5 ops crossing lane boundaries, 512-bit, via 193 zmm compare) + native_lane_ops_matches_interp (vinserti32x4 lane placement + valignd concatenation order confirmed on real CPU — the risky operand-order assumption). Compat regenerated. Suite 385/385, clippy+fmt clean.
<!-- SECTION:NOTES:END -->
