---
id: TASK-134
title: >-
  Hybrid threaded clock: virtual monotonic value + real host blocking (unblocks
  JIT under net/http)
status: Done
assignee: []
created_date: '2026-07-06 17:40'
updated_date: '2026-07-07 07:05'
labels:
  - go-caddy
dependencies: []
ordinal: 143000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The threaded clock (decision-4) anchors guest CLOCK_MONOTONIC to real host time. That assumes the guest runs near real-time -- true for the interpreter, false for the JIT (cold-compiles every block, ~100-400x slower than realtime). Measured: two adjacent time.Now() reads = native 50ns / interp 45us / JIT 19ms of real host time. So guest-perceived monotonic time RACES under the JIT and any time-based Go coordination (net/http deadlines/timers/timeouts) blows instantly -- the go-caddy P5 JIT leg never writes its HTTP response despite accept/epoll/read/handler/serialization all being correct (proven: httptest.Recorder output is byte-identical interp==jit; GOMAXPROCS=1 and GOAMD64=v1 both still fail). Micro-repro: 'for time.Since(start)<30ms {n++}' -> interp n>0, JIT n==0. Proposed fix: return a VIRTUAL monotonic value (fixed quantum per read + advanced by nanosleep/timeout durations, decoupled from host wall-time) while keeping REAL host blocking/yield for nanosleep/futex/epoll so cross-thread sync stays real and timers fire in virtual time. Revises decision-4. DoD: go-caddy P5 go_http JIT leg serves index.html (un-ignore go_http_serves_index_jit); differential threaded timing stays non-asserted; single-threaded virtual clock unchanged.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE via the VCLK track (tasks 141-144, doc-28, decision-6). Delivered a rate-controlled virtual monotonic mt clock (shared AtomicU64: per-read quantum MT_CLOCK_TICK_NS=100ns + idle-only wait credits) decoupled from host wall-time, with nanosleep/futex/epoll still really blocking. Eager-JIT go_http now serves index.html (was 100% empty). KEY CORRECTIONS vs the original proposal (architect review, Fable 5): (1) the wait credit MUST be an idle-only CAS gate (try_advance_from), not fetch_max -- fetch_max re-couples virtual->real for free-running periodic timers (Go sysmon/time.Tick), which kept eager JIT 100% empty at every quantum; (2) the go_http interp load-flake was a SEPARATE non-clock bug (fixture exit-before-flush race in httpserve.go), fixed independently; (3) the premise 'un-ignore go_http_serves_index_jit' was wrong -- that test was never #[ignore]d (it passed via the tier-up dodge); acceptance is instead a new eager-JIT leg (go_http_serves_index_jit_eager). DoD met: eager leg serves 3/3; differential corpus bit-identical (single-threaded clock unchanged); threaded timing non-asserted. Docs: doc-28 revised, decision-6 (proposed -> maintainer ratifies), status/deferred updated. Micro-repro (30ms loop) not added -- eager leg + unit tests sufficed (maintainer approved skipping the deadline gate).
<!-- SECTION:NOTES:END -->
