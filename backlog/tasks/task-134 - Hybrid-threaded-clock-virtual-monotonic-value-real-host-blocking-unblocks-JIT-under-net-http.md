---
id: TASK-134
title: >-
  Hybrid threaded clock: virtual monotonic value + real host blocking (unblocks
  JIT under net/http)
status: To Do
assignee: []
created_date: '2026-07-06 17:40'
updated_date: '2026-07-06 20:07'
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
Planned 2026-07-06 (Fable 5 architect session). Design: backlog/docs/design/threaded-clock-plan.md (doc-28) — rate-controlled virtual monotonic mt clock: one Arc<MtClock> (AtomicU64) shared shim<->ThreadShared; advances by (1) 10us quantum per clock read (fetch_add), (2) completed real-sleep durations, (3) expired futex/epoll timeouts — both credited driver-side as fetch_max(entry+dur); real blocking (Sleep/FutexWait/EpollWait outcomes) unchanged. Decision-6 drafted (proposed) superseding decision-4's clock domain; maintainer ratifies. Implementation sequenced as task-141 (VCLK-1 inert plumbing) -> 142 (the switch) -> 143 (eager-JIT go_http leg + load de-flake evidence) -> 144 (docs + ratification). NOTE a DoD correction: go_http_serves_index_jit is NOT #[ignore]d — it passes via the .tier_up(Some(50)) dodge (go_http.rs:64); the real acceptance is a new eager-JIT leg (no tier-up), see VCLK-3 + open decision 3. Open decisions for maintainer in the plan doc: supersede-vs-amend (recommend supersede), MT_CLOCK_TICK_NS value (recommend 10us), tier-up-dodge fate, Yield credit (recommend none), micro-repro guest (recommend yes).
<!-- SECTION:NOTES:END -->
