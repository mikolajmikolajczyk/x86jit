---
id: TASK-245
title: >-
  lift AVX-128 float siblings: MOVDDUP/MOVSLDUP/MOVSHDUP, BLENDPS/PD imm, DPPD +
  VDPPS/VDPPD
status: To Do
assignee: []
created_date: '2026-07-14 21:51'
updated_date: '2026-07-15 14:38'
labels:
  - lift
  - avx
  - sse3
dependencies: []
ordinal: 274000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Deferred from task-244 (kept that pass focused on the vhaddpd blocker cluster). All genuinely absent, each needs a NEW IR op (per Explore audit): (1) MOVDDUP (F2 0F 12)/MOVSLDUP (F3 0F 12)/MOVSHDUP (F3 0F 16) + VEX — single-source lane-duplication; could reuse VShufps for movsl/shdup but movddup dup-low-f64 needs its own; (2) BLENDPS (66 0F3A 0C)/BLENDPD (0D) + VEX — immediate per-float-lane blend; VBlendD/VBlendW are integer, need a float-lane blend op or extend VBlendD; (3) DPPD (66 0F3A 41) + VDPPS/VDPPD — VDpps IR is f32-only, DPPD needs f64 dot-product IR; VDPPS can reuse VDpps but needs VEX lift dispatch. Only implement when a runtime actually hits them (diagnose real blocker first, like task-243/244).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
MOVDDUP/MOVSLDUP/MOVSHDUP (+VEX) subset done in task-253. Remaining here: BLENDPS/PD imm, DPPD, VDPPS/VDPPD.
<!-- SECTION:NOTES:END -->
