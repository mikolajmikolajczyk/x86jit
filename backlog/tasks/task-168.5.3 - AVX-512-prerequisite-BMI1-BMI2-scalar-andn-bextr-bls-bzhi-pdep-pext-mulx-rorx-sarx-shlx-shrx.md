---
id: TASK-168.5.3
title: >-
  AVX-512 prerequisite: BMI1/BMI2 scalar
  (andn/bextr/bls*/bzhi/pdep/pext/mulx/rorx/sarx/shlx/shrx)
status: To Do
assignee: []
created_date: '2026-07-08 19:19'
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
