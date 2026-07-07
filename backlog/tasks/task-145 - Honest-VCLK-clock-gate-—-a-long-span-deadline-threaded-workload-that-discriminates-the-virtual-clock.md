---
id: TASK-145
title: >-
  Honest VCLK clock gate — a long-span-deadline threaded workload that
  discriminates the virtual clock
status: To Do
assignee: []
created_date: '2026-07-07 07:40'
updated_date: '2026-07-07 10:07'
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
- [ ] #1 A threaded test that FAILS under a fetch_max credit (or host-anchored clock) and PASSES under the idle-only CAS gate — discrimination demonstrated, not merely assert-pass
- [ ] #2 Non-flaky: stable pass margin across >=20 runs, no CPU-load sensitivity
- [ ] #3 Optional cheap tripwire: the doc-28 30ms micro-repro (for time.Since(start)<30ms {n++} terminates n>0 on both backends) for I3/I5
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
