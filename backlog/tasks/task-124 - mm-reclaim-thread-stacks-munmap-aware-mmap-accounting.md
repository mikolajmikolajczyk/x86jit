---
id: TASK-124
title: 'mm: reclaim thread stacks (munmap-aware mmap accounting)'
status: In Progress
assignee: []
created_date: '2026-07-06 12:51'
updated_date: '2026-07-12 19:11'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 133000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope split from P2.4. The mmap bump allocator never reclaims; a thread-churning server leaks guest address space. Irrelevant for bounded-thread acceptance programs (pthreads.elf). Task: munmap-aware mmap accounting.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 mm test: spawn+join N threads in a loop — mmap accounting shrinks after joins (no monotonic growth assert)
<!-- AC:END -->
