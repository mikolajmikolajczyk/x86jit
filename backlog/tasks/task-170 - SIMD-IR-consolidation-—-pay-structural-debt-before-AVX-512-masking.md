---
id: TASK-170
title: SIMD IR consolidation — pay structural debt before AVX-512 masking
status: Done
assignee: []
created_date: '2026-07-08 20:24'
updated_date: '2026-07-09 08:22'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:refactor'
  - seq-2
dependencies: []
ordinal: 190000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
68/163 IrOp variants are vectors (42%), ~15 are width-name duplicates (VLoad256/512...), interp+codegen ~6165 LoC with near-duplicate arms. The next AVX-512 chunk (masking, 168.5.5) is a modifier on ALL ops, not one op — done naively it doubles the vector op count. Consolidate at this phase boundary (post-CpuFeatures) before extending. Parent of the 3 moves below. Blocks 168.5.5; 168.5.1-.4 can proceed on current shape or after. Ref: decision-12, plan discussion 2026-07-08.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Masking represented as a cross-cutting mask-spec + shared write-under-mask helper, NOT per-op masked variants
- [ ] #2 Vector data-mov/logic/packed families width-parameterized (bytes/lanes field), 256/512 name-variants collapsed where it removes duplication
- [ ] #3 Central register-file accessor (read_vec / write_vec_masked over xmm/ymm_hi/zmm_hi) used by generic + masked paths
- [ ] #4 Zero behavior change: full non-fuzz suite + compat green; jit==interp unchanged
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
seq-2 complete: 170.1 masking, 170.2 width-collapse, 170.3 accessor (seq-1), 170.4 cranelift helpers, 172 lift — all shipped. with_vec_cast skipped (closure noise worsens readability). SIMD structural debt paid; AVX-512 masked-data lifts (168.5.5) unblocked.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
