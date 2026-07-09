---
id: TASK-168.5.2
title: 'AVX-512: EVEX logic vpxorq/vpandq/vpord/vpandnq + vpternlog{d,q}'
status: Done
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 17:53'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 185000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
EVEX-encoded bitwise logic (vpxorq/vpandq/vpord/vpandnq, 128/256/512, masked+unmasked) — route like the EVEX 64-bit min/max did. Plus vpternlog{d,q}: 3-input arbitrary bitwise logic via an 8-bit truth table (new IR op). First post-advertise trap on /usr/bin/true is vpxorq. Priority 2.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 jit_eq_interp(v4) differential snippet per lifted op (vpxorq/vpandq/vpord/vpandnq, vpternlog d/q) incl. a nontrivial ternlog imm8 truth table
- [x] #2 compat map regenerated
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed (not committed). Two new width-generic IR ops (via vec_lanes/set_vec interp, load_lane/store_lane + store_lanes_zeroed_above jit — same shape as VMovWide): VLogicWide{dst,a,b,op,bytes} for vpxor/vpand/vpor/vpandn {d,q} (128/256/512, bitwise so d/q suffix irrelevant unmasked), and VPTernlog{dst,b,c,imm,bytes} (dst is also first source; 8-bit truth table, bitwise per lane). lift: lift_evex_vlogic + lift_vpternlog; dispatch Vpxord/q,Vpandd/q,Vpord/q,Vpandnd/q,Vpternlogd/q. emit_ternlog in cranelift mirrors interp's ternlog (OR of AND-of-selected-polarities over set imm bits). Tests: jit.rs avx512_evex_logic_and_ternlog_match_interp (v4: 4 logic ops 128+256, vpternlog 0x96=a^b^c and 0xE8=majority) jit==interp; native.rs native_evex_logic_ternlog_matches_interp validates vpxorq + vpternlogd 0x96 (128) against the REAL CPU (Unicorn can't decode EVEX), self-skips without avx512vl. Compat map regenerated (delta: 20 EVEX logic/ternlog forms now covered). Full suite 374/374 (--features unicorn), clippy+fmt clean. NOT covered: memory src (deferred, register only), masked/zeroing forms (rejected → belongs with 168.5.5), and 512-bit isn't fully observable in jit_eq_interp until CpuSnapshot grows ZMM (task-193) — the 512 lane loop is identical to 128/256 so covered by construction.
<!-- SECTION:NOTES:END -->
