---
id: TASK-168.5.5
title: 'AVX-512: masked/zeroing EVEX data ops (merge + zero write-masking)'
status: In Progress
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 19:47'
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
Increment 1 (masked EVEX logic) landed; task stays In Progress. The masking mechanism: interp computes op(a,b) per lane then cpu.write_masked (merge/zero per element at k granularity); the JIT routes through a new vmasked_logic_helper → x86jit_core::interp::exec_masked_logic (same code path → jit==interp), mirroring the VMaskMov helper pattern. New IrOp::VMaskedLogic; lift_evex_vlogic now emits it when evex_writemask is Some (elem 4/8 from d/q suffix), else the unmasked VLogicWide (k0). Covers vpxor/vpand/vpor/vpandn {d,q} {k}{z}. Tests: jit avx512_masked_logic_match_interp (merge + zero, all-ones + all-zero masks, 128+256 — leverages 193's zmm/kmask compare) + native_masked_logic_matches_interp (real CPU). Compat regenerated. Suite 383/383, clippy+fmt clean. REMAINING (subsystem not complete): masked PACKED ARITH (vpaddd/vpsubd/vpmind/etc {k}{z}) — same helper pattern, extend op-code space; masked MEMORY moves (vmovdqu32/64 {k} [mem] load/store with fault suppression on masked-off lanes) — the harder part (page-fault suppression), genuinely deferred. Reg-reg masked moves already exist (VMaskMov, task-170.1).
<!-- SECTION:NOTES:END -->
