---
id: TASK-253
title: Lift SSE3 lane-duplicating moves movddup/movsldup/movshdup + VEX.128
status: Done
assignee: []
created_date: '2026-07-15 14:31'
updated_date: '2026-07-15 14:38'
labels:
  - lift
  - m8-simd
dependencies: []
ordinal: 283000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
unemups4 (PS4) needs vmovddup; whole SSE3-dup splat family absent (legacy + VEX). Each is a fixed dword shuffle of one source, so they reuse the existing VShuffle32 op (no new IR op): movsldup=[0,0,2,2] imm 0xA0; movshdup=[1,1,3,3] imm 0xF5; movddup=[0,1,0,1] imm 0x44 (its mem form is an m64 8-byte load; the shuffle reads only dwords 0/1). VEX.128 appends VZeroUpper; ymm/256 defers via reg_xmm None.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 movddup/movsldup/movshdup + Vmovddup/Vmovsldup/Vmovshdup lifted (reg + mem); VEX zeroes 255:128; ymm defers
- [ ] #2 differential vs Unicorn (legacy) + vex_eq_sse/upper-zero (VEX) + jit==interp; ratchet allowlist + coverage regen
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. movddup/movsldup/movshdup + VEX forms lifted as fixed VShuffle32 shuffles (0x44/0xA0/0xF5) — zero new IR op. movddup mem = m64 load (shuffle reads only dwords 0/1). VEX appends VZeroUpper; ymm defers. Tests: movdup_family_match_unicorn (legacy vs CPU, reg+mem), vmovdup_family_vex_eq_sse, movdup_family_match_interp (jit==interp, dirty ymm_hi). Ratchet allowlist + coverage regen. SIDE EFFECT: SSE3 reached 100% lift → removed the now-stale SSE3 cpuid waiver (cpuid-waivers.ron). 728/728, clippy+fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
