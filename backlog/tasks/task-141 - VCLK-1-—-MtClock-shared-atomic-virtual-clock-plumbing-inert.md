---
id: TASK-141
title: 'VCLK-1 — MtClock: shared atomic virtual clock + plumbing (inert)'
status: To Do
assignee: []
created_date: '2026-07-06 20:05'
labels:
  - go-caddy
dependencies:
  - TASK-134
ordinal: 150000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
First phase of task-134 (design: backlog/docs/design/threaded-clock-plan.md, decision-6 draft). Add the MtClock newtype (AtomicU64: tick/peek/advance_to/seed, Relaxed) + MT_CLOCK_TICK_NS const to x86jit-linux/src/shim.rs next to the CLOCK_* consts; mt_clock: Arc<MtClock> field on LinuxShim (Default zero; fresh Arc in the fork ctor, shim.rs:831-873); clock: Arc<MtClock> on ThreadShared, wired in run_threaded (thread.rs:160-164) by cloning shim.mt_clock before Arc-wrapping the shim; seed(clock_ns) at the mt flip (shim.rs:2377) ALONGSIDE the still-authoritative clock_anchor. No behavior change — now_ns untouched.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 MtClock unit tests: tick returns old+quantum; advance_to is a monotone max; concurrent tick/advance_to interleavings never produce a decreasing sample
- [ ] #2 Full test suite unchanged (no behavior change); clippy -D warnings + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
