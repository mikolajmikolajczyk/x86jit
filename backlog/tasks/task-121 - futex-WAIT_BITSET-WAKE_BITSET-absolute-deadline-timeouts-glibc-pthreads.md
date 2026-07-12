---
id: TASK-121
title: 'futex: WAIT_BITSET/WAKE_BITSET + absolute-deadline timeouts (glibc pthreads)'
status: Done
assignee: []
created_date: '2026-07-06 12:51'
updated_date: '2026-07-12 17:12'
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
- [x] #1 mt_shim test: pthread_cond_timedwait (WAIT_BITSET + absolute deadline) wakes correctly and times out correctly
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged a4d3a32). FUTEX_WAIT_BITSET(9)/WAKE_BITSET(10): val3 bitmask (R9) threaded through FutexWait/FutexWake/ThreadShared; plain WAIT/WAKE unify as MATCH_ANY (byte-identical). Replaced per-address generation counter with explicit per-address FutexQueue (id+bitmask+woken, FIFO, bitmask-selective wake). Absolute deadlines: abs_deadline_to_rel converts against the shared virtual clock. REVIEW-CAUGHT CRITICAL (fixed 5949bd6): monotonic deadlines were mis-rebased (base subtracted only for realtime flag) -> a CLOCK_MONOTONIC deadline (glibc >=2.30 default) landed ~54 years out = effective hang; shim's clock_gettime reports base+ns for EVERY clock, so base is now subtracted unconditionally. 3 adversarial reviews (concurrency/robust-list/clock): no other bugs. Perf verified: full suite 123s on the real merge target (GOMAXPROCS>1), go_http fast.
<!-- SECTION:NOTES:END -->
