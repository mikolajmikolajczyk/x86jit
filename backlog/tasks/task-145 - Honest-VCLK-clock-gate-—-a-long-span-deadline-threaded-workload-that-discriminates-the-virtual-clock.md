---
id: TASK-145
title: >-
  Honest VCLK clock gate — a long-span-deadline threaded workload that
  discriminates the virtual clock
status: Done
assignee: []
created_date: '2026-07-07 07:40'
updated_date: '2026-07-07 12:58'
labels:
  - go-caddy
  - 'crate:tests'
  - 'crate:linux'
  - 'goal:test'
dependencies: []
ordinal: 154000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The VCLK track (task-134, decision-6) landed but NO integration test discriminates the idle-only CAS wait credit from either a re-coupling fetch_max credit or a full host-anchored revert. Reasons found empirically during VCLK-3: (1) the go_http eager empty-response was a non-clock FIXTURE bug (exit-before-flush), so the deadline-free eager leg passes under fetch_max AND host-anchored too; (2) a prototyped ReadHeaderTimeout=Nms go_http variant does not discriminate — its accept->read window is too short in guest-progress terms (both credit rules pass >=500ms, VCLK fails <=200ms). The re-coupling only manifests over a LONG span of free-running periodic-timer cycles. Add a threaded guest whose correctness depends on a long-span deadline measured in virtual (read-metered) time, not real time: e.g. an http.Server with a real ReadTimeout/WriteTimeout/IdleTimeout that spans many sysmon/ticker cycles under eager JIT, or a micro-guest: spawn a goroutine, set deadline=now+D, do startup-heavy work spanning several periodic-timer cycles, assert the deadline did NOT blow. Under fetch_max/host-anchored it blows; under the idle-CAS gate it passes. This is the honest task-134 acceptance gate (currently only pinned by the unit test busy_process_expiry_does_not_credit). Recorded as deferred in backlog/docs/deferred.md (VCLK exclusions).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A threaded test that FAILS under a fetch_max credit (or host-anchored clock) and PASSES under the idle-only CAS gate — discrimination demonstrated, not merely assert-pass
- [x] #2 Non-flaky: stable pass margin across >=20 runs, no CPU-load sensitivity
- [x] #3 Optional cheap tripwire: the doc-28 30ms micro-repro (for time.Since(start)<30ms {n++} terminates n>0 on both backends) for I3/I5
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. Deterministic threaded discrimination gate: thread::tests::busy_periodic_timer_discriminates_cas_from_fetch_max replays one long-span busy interleaving (periodic timer + worker read, per-cycle barrier ordering the read strictly between peek and credit) under BOTH credit rules — IdleCas (try_advance_from, prod) injects 0 wall-coupled time (deadline holds), FetchMax (advance_to) re-couples and blows a 320ms deadline. AC#3 tripwire: read_metered_deadline_spin_terminates (doc-28 30ms). Deterministic (no sleeps) -> non-flaky, load-invariant: 0/30 fail under 6x CPU load. Real-guest variant infeasible per AC#2 (eager JIT minutes/run) -> documented in deferred.md. Suite 252 green, clippy+fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
