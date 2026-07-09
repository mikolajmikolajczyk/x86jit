---
id: TASK-168.5.1
title: 'AVX-512: EVEX vpcmpeq/gt/neq{b,d} -> opmask (masked compares)'
status: Done
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 17:36'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 184000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Dedicated-opcode EVEX masked compares that write an opmask: vpcmpeqb/eqd/gtb/neqb/neqd (glibc's #1 AVX-512 op, ~2000 uses in string/mem routines). iced names them Vpcmpeqb etc but with a k destination + EVEX writemask; currently mis-routed to the packed-bin (xmm) path -> traps. Reuse the vpcmp->k machinery (VPCmpToMask, task-168.5) with the fixed EQ/GT predicate + writemask. Priority 1.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 jit_eq_interp(v4) differential snippet per lifted compare (vpcmpeqb/eqd/gtb/neq*) incl. write-masked variants
- [x] #2 fuzzer or hand differential validates opmask register results, not just vector state
- [x] #3 compat map regenerated with the new EVEX compares
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed (not yet committed). lift.rs: vpcmpeq{b,w,d}/vpcmpgt{b,w,d} now branch on the destination — a k-register dest (EVEX form) routes to the existing VPCmpToMask machinery with the opcode's fixed predicate (EQ=0 / signed GT=6); anything else keeps the legacy/VEX packed path. New lift_vpcmp_fixed_or_packed helper; register src2 only (memory deferred, matching lift_vpcmp). Both interp + cranelift already lower VPCmpToMask, so jit==interp is free. Tests: jit.rs avx512_vpcmpeq_gt_to_mask_match_interp (jit_eq_interp v4; eq/gt, byte+dword lanes, 128+256-bit, a write-masked variant — mask moved to GPR via kmovd so the opmask RESULT is compared, not just vector state). native.rs native_evex_vpcmpeqb_matches_interp: the EVEX compare validated against the REAL CPU via the NativeOracle (task-186) — Unicorn can't decode EVEX, so this is the only hardware check of the interp's opmask semantics; mask 0xFFFB confirmed on silicon; self-skips without avx512bw. Compat map regenerated: no delta (these mnemonics were already recorded present via the old packed routing; this is a correctness reroute). Full suite 372/372 (--features unicorn), clippy+fmt clean. NOT covered here: memory src2, q-lane forms (vpcmpeqq/gtq), and dedicated neq (NEQ is the immediate vpcmp pred=4 path, already lifted).
<!-- SECTION:NOTES:END -->
