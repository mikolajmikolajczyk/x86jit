---
id: TASK-168.5.4
title: >-
  AVX-512 prerequisite: SSE4.2 pcmpistri/pcmpestri (+ SSE4.1
  pmovzx/blendv/pmulld/round/ptest)
status: Done
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 20:31'
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
- [x] #1 differential snippets for pcmpistri/pcmpestri (ECX index + flags) and each SSE4.1 op (pmovzx/blendv/pmulld/round/ptest)
- [x] #2 native_matches_interp oracles them on real hardware (SSE4 decodes fine in every oracle)
- [x] #3 compat map regenerated
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
COMPLETE (3 increments). Inc1: pmovzx/pmovsx + pmulld. Inc2: blendv + round + dword min/max (+ round_ties_even signed-zero fix). Inc3 (pcmpistri/pcmpestri): interp pcmpstr() implements the full Intel string-aggregation with the per-(i,j) validity-override table (EqualAny/Ranges/EqualEach/EqualOrdered), polarity, LSB/MSB index, CF/ZF/SF/OF; pcmpstr_run reads xmm+EAX/EDX/implicit-null lengths. JIT via a read-only pcmpstr helper (out-slot [ecx, packed flags]); codegen writes ECX via write_gpr + flags via store_flag (like BMI). Lift: Pcmpistri/Pcmpestri -> VPcmpStr (register src2 only). VALIDATION: native_pcmpstr_fuzz_matches_interp fuzzes ALL 96 imm8 combos (agg x polarity x format x sign x msb) x random inputs x istri/estri against the REAL CPU — the interp matches hardware everywhere (Unicorn can't verify this). jit==interp caught + fixed a JIT flag-unpack bug (missing &1 mask). sse42_pcmpstr_match_interp for the jit path. Fixed unknown_instruction_reports_real_bytes to use dpps (pcmpistri now lifted). Compat regenerated. Suite 387/387, clippy+fmt clean. NOT lifted (out of scope, noted): pcmpistrm/pcmpestrm (mask forms), pmuldq/pmuludq, insertps, dpps/dppd, pblendw, memory src2.
<!-- SECTION:NOTES:END -->
