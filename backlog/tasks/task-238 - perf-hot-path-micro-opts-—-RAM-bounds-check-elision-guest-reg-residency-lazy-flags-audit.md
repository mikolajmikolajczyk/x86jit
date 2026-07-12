---
id: TASK-238
title: >-
  perf: hot-path micro-opts — RAM bounds-check elision, guest-reg residency,
  lazy flags audit
status: To Do
assignee: []
created_date: '2026-07-12 20:21'
labels:
  - 'crate:cranelift'
  - 'goal:perf'
milestone: ps4-perf
dependencies: []
ordinal: 267000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tier-5 (doc-33). Bundle of hot-path micro-optimizations for the games profile: (1) elide guest->host RAM bounds checks where the access is provably in-region (flat/single-region fast path); (2) keep hot guest GPRs/XMMs resident in host regs across a block instead of reloading from CpuState each block; (3) audit lazy/elided EFLAGS computation coverage (games do heavy arithmetic; unused flag computation is waste). Each gated by a game-shaped microbench delta + unicorn correctness.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 at least one of {RAM bounds-elision, reg residency, flag elision} lands with a measured microbench speedup and bit-exact unicorn validation; others filed as sub-tasks if deferred
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
