---
id: TASK-134
title: >-
  Hybrid threaded clock: virtual monotonic value + real host blocking (unblocks
  JIT under net/http)
status: To Do
assignee: []
created_date: '2026-07-06 17:40'
updated_date: '2026-07-06 19:49'
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
Evidence 2026-07-06: go_http/go_net are LOAD-FLAKY via this same clock. Under host load ~3.7 the interp guest itself runs < realtime, so the host-anchored monotonic clock races from the guest's view and net/http deadlines blow -> empty response (fast-fail 0.34s). Passes reliably on an idle host. So the virtual-monotonic threaded clock (this task) also de-flakes the go-caddy tests, not just the JIT leg. Repro: run go_http_serves_index_interp while the box is loaded.
<!-- SECTION:NOTES:END -->
