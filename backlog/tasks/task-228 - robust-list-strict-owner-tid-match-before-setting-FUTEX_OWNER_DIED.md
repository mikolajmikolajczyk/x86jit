---
id: TASK-228
title: 'robust-list: strict owner-tid match before setting FUTEX_OWNER_DIED'
status: In Progress
assignee: []
created_date: '2026-07-12 17:12'
updated_date: '2026-07-12 18:39'
labels:
  - 'crate:linux'
  - 'goal:fidelity'
dependencies: []
ordinal: 257000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-122 review fidelity gap. walk_robust_list (thread.rs) ORs FUTEX_OWNER_DIED into every listed entry's futex word without checking word & FUTEX_TID_MASK == dying_tid, unlike the real kernel. For a correct glibc program the per-thread robust list only holds mutexes this thread owns, so it's benign; a malicious/buggy guest could over-flag a word a live sibling holds (still sandbox-internal, only affects its own EOWNERDEAD recovery). Add the tid check for strict fidelity.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 walk_robust_list sets FUTEX_OWNER_DIED only when the word's low-30 TID bits equal the dying thread's tid; a mismatched word is left untouched
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
