---
id: TASK-120
title: >-
  CR — efficiency: interp temps zero-fill / fresh_code_pages / trap_window
  recompute
status: To Do
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: code-review
dependencies: []
ordinal: 129000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
interp.rs zero-fills the whole temps scratch per block dispatch (SSA define-before-use makes it unneeded); fresh_code_pages builds ~1M AtomicBool element-by-element at Reserved startup; vm.rs recomputes trap_window (full region scan) per block materialize.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
