---
id: TASK-142
title: >-
  VCLK-2 — the switch: virtual mt clock value + driver wait credits (decision-6
  lands)
status: To Do
assignee: []
created_date: '2026-07-06 20:06'
labels:
  - go-caddy
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

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
