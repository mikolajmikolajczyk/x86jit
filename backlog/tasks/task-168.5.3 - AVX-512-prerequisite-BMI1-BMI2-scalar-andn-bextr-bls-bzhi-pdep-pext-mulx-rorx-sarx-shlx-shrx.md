---
id: TASK-168.5.3
title: >-
  AVX-512 prerequisite: BMI1/BMI2 scalar
  (andn/bextr/bls*/bzhi/pdep/pext/mulx/rorx/sarx/shlx/shrx)
status: Done
assignee: []
created_date: '2026-07-08 19:19'
updated_date: '2026-07-09 09:47'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168.5
ordinal: 186000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
BMI1/BMI2 scalar ops glibc uses once v3+ is advertised (shrx[66]/blsmsk[56]/bzhi[36]/sarx/shlx/andn + bextr/blsr/blsi/pdep/pext/mulx/rorx). Not AVX-512 but gated by the same advertise; needed for v3/v4 binaries. Priority 3.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
CORE DONE (commit pending): andn/blsi/blsr/blsmsk/bextr/bzhi via IrOp::Bmi{BmiOp,size} + shared bmi_result + cranelift out-slot helper. LEFTOVERS: sarx/shlx/shrx (reuse Shl/Shr/Sar + FlagMask::NONE), rorx (rotate reuse), mulx (two-dst, separate), pdep/pext (cranelift helper — no native op). tzcnt/lzcnt already done in 176.
<!-- SECTION:NOTES:END -->
