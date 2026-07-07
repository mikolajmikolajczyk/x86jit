---
id: TASK-142
title: >-
  VCLK-2 — the switch: virtual mt clock value + driver wait credits (decision-6
  lands)
status: Done
assignee: []
created_date: '2026-07-06 20:06'
updated_date: '2026-07-07 10:01'
labels:
  - go-caddy
  - 'crate:linux'
dependencies:
  - TASK-141
ordinal: 151000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The feature (task-134, threaded-clock-plan.md M2-M5). now_ns's mt arm (shim.rs:747-755) returns mt_clock.tick(MT_CLOCK_TICK_NS); DELETE clock_anchor (field shim.rs:609, flip write shim.rs:2377, fork-ctor init shim.rs:867, doc comments). Driver credits (thread.rs), all as clock.advance_to(entry + dur) with entry peeked after the shim guard drops: Sleep arm (thread.rs:253-265) after the chunked sleep completes (not the exited early-out); FutexWait timeout arm on ETIMEDOUT only; EpollWait timeout arm on the deadline/Rax=0 path only (thread.rs:288). Wakes/readiness/exited credit nothing; chunked loops credit only at final expiry; Yield credits nothing. Rewrite the flip unit test (shim.rs:2914-2931): ST tick unchanged, flip seeds the mt clock, mt reads tick the quantum monotonically, a credited wait advances at least its duration. Single-threaded paths bit-identical (I1).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 mt.rs + go_http/go_net interp and tiered-JIT legs green (real blocking preserved, timers fire)
- [ ] #2 Full differential corpus green — proves the single-threaded clock is bit-identical
- [ ] #3 thread.rs unit tests: expired futex timeout advances the shared clock >= timeout; wake-before-timeout does not advance; concurrent readers stay monotone
- [ ] #4 OCI multiprocess suite green (escalation path shares the clock, R6)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
VCLK-2 landed with Fable-architect CAS-gate correction. now_ns mt arm -> mt_clock.tick(); clock_anchor deleted; driver credits expired Sleep/FutexWait/EpollWait via idle-only CAS gate (MtClock::try_advance_from) — credit lands only when no other guest thread moved the clock during the wait, so free-running periodic timers (Go sysmon/time.Tick) fire on read-metered virtual time instead of re-coupling to host wall-rate. Fable review found the original fetch_max credit re-coupled virtual<->real for periodic waiters (eager JIT stayed 100% empty at every quantum, @10us regressed interp). Also fixed the go_http fixture exit-vs-flush race (non-clock) and added a permanent eager-JIT leg. Results: eager JIT 0->3/3; interp ~10-20%->20/20; diff corpus bit-identical; 289/289 suite, clippy+fmt clean. MT_CLOCK_TICK_NS=100ns. doc-28 revised (M2 CAS box, M4/I5, R7). AC/DoD met.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
