---
id: TASK-168.4
title: 'AVX-4: CPUID advertise AVX/AVX2 + amend decision-2'
status: To Do
assignee: []
created_date: '2026-07-08 15:21'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies:
  - TASK-168
parent_task_id: TASK-168
ordinal: 181000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Once 128+256 lifting is solid, flip cpuid_run to advertise AVX (+AVX2/BMI as covered) so glibc IFUNC selects the AVX paths, and write a decision amending decision-2 (which currently masks SSE4+ to force SSSE3). Gate LAST — advertising before lifting is solid exposes unrunnable paths.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 CPUID advertises AVX/AVX2; the differential corpus (busybox/alpine/glibc/sqlite/lua/cpython + native oracle) stays green with glibc now selecting AVX string routines; a decision doc records the change
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
