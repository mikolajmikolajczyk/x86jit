---
id: TASK-209
title: >-
  Lift AVX-512 misc from real binaries: vprold/vprolq, vplzcntd/q, vpblendmd/q,
  vpmultishiftqb, vshuff32x4, vpconflictd
status: To Do
assignee: []
created_date: '2026-07-10 22:55'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 238000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Objdump of real v4 binaries shows these EVEX ops present + unlifted: vprold 157, vplzcntd 156, vpmultishiftqb 124 (VBMI), vpblendmd 115, vshuff32x4, vpconflictd (host has avx512cd+avx512vbmi). Rotates (vprold/q), leading-zero-count (vplzcntd/q), mask-blend (vpblendmd/q), multishift (vpmultishiftqb), 128-bit-lane shuffle (vshuff32x4), conflict-detect (vpconflictd). Lift via the established EVEX helper→interp pattern; native bit-exact (host is v4 with these features). Found via 2026-07-11 trap-and-fix recon; batched after AES/SHA.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
