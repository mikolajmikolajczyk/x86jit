---
id: TASK-201
title: 'AVX-512/FMA: vfmadd/vfmsub/vfnmadd/vfnmsub {132,213,231}{ss,sd,ps,pd}'
status: To Do
assignee: []
created_date: '2026-07-10 14:48'
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
- [ ] #1 vfmadd/sub scalar+packed lifted (132/213/231 orders)
- [ ] #2 vfnmadd/vfnmsub variants lifted
- [ ] #3 memory src + masked EVEX forms
- [ ] #4 differential + native cross-check; compat regen; suite green; clippy+fmt
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
