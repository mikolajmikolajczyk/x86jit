---
id: TASK-121
title: 'futex: WAIT_BITSET/WAKE_BITSET + absolute-deadline timeouts (glibc pthreads)'
status: To Do
assignee: []
created_date: '2026-07-06 12:51'
updated_date: '2026-07-09 15:10'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 130000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope split from P2.3. musl (pthreads.elf) and Go use plain FUTEX_WAIT/WAKE; glibc pthreads uses the *_BITSET ops and absolute CLOCK_REALTIME deadlines. handle_mt currently no-op-succeeds unknown futex ops (Rax=0) — a WAIT-class op returning instantly is a spin-loop generator. Interim (done in P2.8): gap-log unknown futex WAIT-class ops instead of silent success. This task: implement bitset ops + absolute→relative deadline conversion.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 mt_shim test: pthread_cond_timedwait (WAIT_BITSET + absolute deadline) wakes correctly and times out correctly
<!-- AC:END -->
