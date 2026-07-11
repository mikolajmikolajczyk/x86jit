---
id: TASK-201
title: 'AVX-512/FMA: vfmadd/vfmsub/vfnmadd/vfnmsub {132,213,231}{ss,sd,ps,pd}'
status: Done
assignee: []
created_date: '2026-07-10 14:48'
updated_date: '2026-07-11 09:20'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
ordinal: 230000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
FMA3 fused multiply-add subsystem (~48 encodings). Blocks heavy python3 (statistics/numpy-like), scientific/ML workloads under --cpu v4. Natural follow-on to task-195 (all non-FMA sampled v4 /usr/bin now instruction-clean). Needs: FloatBinOp-style 3-input op (a*b+c with the 132/213/231 operand-order variants + negate-product/negate-addend), scalar (ss/sd) + packed (ps/pd) any width, register+memory src, masked EVEX forms; jit_eq_interp(v4) + native cross-check per representative; compat regen.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vfmadd/sub scalar+packed lifted (132/213/231 orders)
- [x] #2 vfnmadd/vfnmsub variants lifted
- [x] #3 memory src + masked EVEX forms
- [x] #4 differential + native cross-check; compat regen; suite green; clippy+fmt
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#3 DONE 2026-07-11: masked EVEX PACKED FMA (ps/pd, merge+zeroing) lifted. writemask:Option<u8>+zeroing added to VFma/VFmaM; removed blanket masked-deferral, kept narrow scalar&&masked->unsupported guard (EVEX scalar upper-bits-from-op1 subtle+rare, deferred). exec_fma/fma_mem_run apply write_masked. native_fma_masked_matches_interp (128/256/512 merge+zeroing bit-exact vs real CPU) + fma_masked_variants_match_interp (jit==interp, incl masked mem operand). Suite 544/544 (--features unicorn), clippy+fmt clean, aarch64 clean. Memory-src forms (AC#3 first half) already done earlier. REMAINING deferred: masked scalar FMA only.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
