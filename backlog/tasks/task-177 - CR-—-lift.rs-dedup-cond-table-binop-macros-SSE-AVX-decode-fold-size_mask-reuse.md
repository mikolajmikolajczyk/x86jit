---
id: TASK-177
title: >-
  CR — lift.rs dedup: cond-table + binop macros, SSE/AVX decode fold, size_mask
  reuse
status: Done
assignee: []
created_date: '2026-07-09 09:56'
updated_date: '2026-07-09 10:28'
labels:
  - CR
dependencies: []
ordinal: 201000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Code-quality consolidation in x86jit-core/src/lift.rs. No behavior change; jit==interp preserved. (A) jcc_cond/setcc_cond/cmovcc_cond are the same 16-arm mnemonic->Cond table three times (48 arms) -> one macro. (B) mk_binop is a ~100-line struct-literal switch over 14 BinOp variants all building IrOp::X{dst,a,b,size,set_flags} identical but the ctor name -> macro_rules. (C) SSE/AVX operand-decode preamble (reg d, match src{reg-form|mem-form|Err}) copy-pasted ~8x incl near-identical _avx YMM twins -> shared dispatch helper/macro (extends reg_extractor! direction). (D) emit_mem_bt re-derives (1<<n)-1 -> call existing size_mask. Verify: build + cargo nextest run -E 'not binary(fuzz_robustness)' + clippy.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 cond tables folded to one macro,mk_binop folded to macro,SSE/AVX decode preamble shared,emit_mem_bt reuses size_mask,full suite green + clippy clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
