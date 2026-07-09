---
id: TASK-168.5.4
title: >-
  AVX-512 prerequisite: SSE4.2 pcmpistri/pcmpestri (+ SSE4.1
  pmovzx/blendv/pmulld/round/ptest)
status: In Progress
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 19:09'
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
Increment 2 landed (task stays In Progress). blendv (blendvps/blendvpd/pblendvb, mask=implicit XMM0) via lift_blendv → VPBlendV/M (jit sshr_imm+bitselect). round{ps,pd,ss,sd} via lift_round → VPRound/M (imm8 mode → nearest/floor/ceil/trunc; jit cranelift nearest/floor/ceil/trunc + extractlane/insertlane for scalar). pmin/max sd/ud reuse packed MinS/MaxS/MinU/MaxU at lane 4. BUG FIXED: round_ties_even(-0.5) returned +0.0; hardware+cranelift give -0.0 — now copysign the input sign on zero result (differential caught it). Tests: sse41_blendv_round_match_interp, sse41_dword_minmax_match_interp, native_sse41_round_blendv_matches_interp (roundps(-0.5)=-0.0 on real CPU). Suite 379/379, clippy+fmt clean. REMAINING: increment 3 = pcmpistri/pcmpestri (helper→interp); bonus decision-2 non-AC = pmuldq/pmuludq, pblendw, insertps.
<!-- SECTION:NOTES:END -->
