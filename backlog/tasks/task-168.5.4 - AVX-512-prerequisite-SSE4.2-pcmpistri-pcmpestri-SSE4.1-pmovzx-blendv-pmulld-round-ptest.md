---
id: TASK-168.5.4
title: >-
  AVX-512 prerequisite: SSE4.2 pcmpistri/pcmpestri (+ SSE4.1
  pmovzx/blendv/pmulld/round/ptest)
status: In Progress
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 18:56'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 187000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
SSE4.2 string-compare aggregation ops (pcmpistri[204]/pcmpestri) + the SSE4.1 gaps decision-2 dropped. Needed because advertising v2+ makes glibc IFUNC + inline code select them. Complex aggregation semantics. Priority 4.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 differential snippets for pcmpistri/pcmpestri (ECX index + flags) and each SSE4.1 op (pmovzx/blendv/pmulld/round/ptest)
- [ ] #2 native_matches_interp oracles them on real hardware (SSE4 decodes fine in every oracle)
- [ ] #3 compat map regenerated
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Increment 1 landed (task stays In Progress). pmovzx/pmovsx {bw,bd,bq,wd,wq,dq} (reg+mem source) via lift_pmovx → new VPMovExtend/VPMovExtendM (interp pmov_extend byte-wise; jit emit_pmov_extend = bitcast + chained uwiden_low/swiden_low). pmulld via lift_vpacked_bin + new PackedBinOp::MulLo32 (interp wrapping_mul&mask; jit vector imul). Tests: jit.rs sse41_pmovx_pmulld_match_interp (6 forms zero/sign, mem source, pmulld) + native.rs native_sse41_pmovsx_pmulld_matches_interp (real CPU). Compat regenerated. Suite 376/376, clippy+fmt clean. REMAINING: increment 2 = blendv/pblendw/round/insertps/pmin-max sd-ud/pmuldq-pmuludq; increment 3 = pcmpistri/pcmpestri (helper→interp string aggregation).
<!-- SECTION:NOTES:END -->
