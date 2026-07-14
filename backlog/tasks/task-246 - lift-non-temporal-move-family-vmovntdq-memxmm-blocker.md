---
id: TASK-246
title: 'lift non-temporal move family (vmovntdq [mem],xmm blocker)'
status: Done
assignee: []
created_date: '2026-07-14 21:55'
updated_date: '2026-07-14 22:04'
labels:
  - lift
  - avx
  - sse2
dependencies: []
ordinal: 275000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
unemups4 Mono/libc bring-up hits vmovntdq %xmm0,(%rdi) (VEX.128.66.0F E7 /r, non-temporal 128-bit store xmm->[mem]). Diagnosis: non-temporal is a cache HINT only — semantically a plain aligned move. Legacy Movntdq/Movntps/Movntpd already lift (route to lift_vmov, task-164) and Movnti already lifts; the gap is (a) legacy Movntdqa (66 0F38 2A, non-temporal LOAD [mem]->xmm) and (b) all VEX forms Vmovntdq (66 0F E7 store) / Vmovntps (0F 2B store) / Vmovntpd (66 0F 2B store) / Vmovntdqa (66 0F38 2A load) — none dispatched. Fix: route Movntdqa to lift_vmov(16) like Movdqa; route the VEX forms to lift_vmov_avx(4) like Vmovdqa (handles store/load + VEX.128 upper-zeroing on load). Pure decode-dispatch, no new IR. Differential tests: legacy-vs-Unicorn + VEX vex_eq_sse incl exact blocker vmovntdq [mem],xmm.
<!-- SECTION:DESCRIPTION:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Add Movntdqa to the legacy aligned-move arm (lift_vmov 16). Add Vmovntdq/Vmovntps/Vmovntpd/Vmovntdqa to the VEX aligned-move arm (lift_vmov_avx 4). No new IR — reuses VLoad/VStore/VMov + VZeroUpper. Tests: legacy-vs-Unicorn (movntdqa load + movntdq/ps/pd stores already covered — add movntdqa), VEX vex_eq_sse (incl blocker vmovntdq [mem],xmm + load upper-zero). Ratchet ALLOWLIST + compat regen.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Diagnosis: pure decode-dispatch gap. Non-temporal = cache hint only → plain aligned move. Legacy Movntdq/Movntps/Movntpd already lifted (task-164) and Movnti already lifted; missing were legacy Movntdqa (aligned NT load) + all VEX forms (Vmovntdq/Vmovntps/Vmovntpd stores, Vmovntdqa load). Fix: added Movntdqa to the legacy aligned-move arm (lift_vmov 16) and the 4 VEX forms to the VEX aligned-move arm (lift_vmov_avx 4). Reuses VLoad/VStore/VMov + VZeroUpper (load upper-zero via lift_vmov_vex) — NO new IR op, no interp/cranelift changes. Tests: movntdqa_load_matches_unicorn, vex128_movnt_moves (incl exact blocker vmovntdq [mem],xmm + ntps/ntpd stores + vmovntdqa load), vmovntdqa_load_zeroes_ymm_upper, jit movnt_moves_match_interp. Full suite 497 passed/3 skipped (--features unicorn, minus fuzz_robustness); clippy+fmt clean. Coverage ratchet + compat map UNCHANGED (the probe already classified these mnemonics as lifted via the shared aligned-move Code representatives). No skips — whole family covered.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
