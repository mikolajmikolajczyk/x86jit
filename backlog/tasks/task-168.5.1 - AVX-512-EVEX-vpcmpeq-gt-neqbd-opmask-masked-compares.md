---
id: TASK-168.5.1
title: 'AVX-512: EVEX vpcmpeq/gt/neq{b,d} -> opmask (masked compares)'
status: To Do
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 15:10'
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
- [ ] #1 jit_eq_interp(v4) differential snippet per lifted compare (vpcmpeqb/eqd/gtb/neq*) incl. write-masked variants
- [ ] #2 fuzzer or hand differential validates opmask register results, not just vector state
- [ ] #3 compat map regenerated with the new EVEX compares
<!-- AC:END -->
