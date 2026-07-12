---
id: TASK-168.5.5
title: 'AVX-512: masked/zeroing EVEX data ops (merge + zero write-masking)'
status: Done
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-12 13:37'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 188000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The per-element masking subsystem: vmovdqu32/64{k}{z} + masked arithmetic/logic with merge (keep dst) vs zero semantics under a k write-mask (303 {k} sites in glibc). The one real subsystem among the AVX-512 gaps. Priority 5 (evex_is_masked currently -> unsupported for data ops).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 jit_eq_interp(v4) differential covers merge-masking AND zero-masking per lifted data op (k0 vs kN, {z} vs merge)
- [ ] #2 edge case: all-zero mask and all-ones mask snippets included
- [ ] #3 compat map regenerated
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE via task-215. The remaining items this task listed — masked PACKED ARITH (vpaddd/vpsubd/vpmin*/vpmul* {k}{z}) and masked MEMORY moves (vmovdqu32/64 {k}[mem] with page-fault suppression on masked-off lanes) — all landed in task-215: VMaskedPacked (op-code space extended to codes 0-19), VMaskedShift, VMaskLoadMem/VMaskStoreMem (element-wise so masked-off lanes never touch memory = fault suppression). jit==interp + native tests exist; masked EVEX crypto runs real openssl/TLS under v4. Increment-1 masked logic (VMaskedLogic) was already done.
<!-- SECTION:NOTES:END -->
