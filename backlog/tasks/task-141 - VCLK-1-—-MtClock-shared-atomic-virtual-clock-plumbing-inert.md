---
id: TASK-141
title: 'VCLK-1 — MtClock: shared atomic virtual clock + plumbing (inert)'
status: Done
assignee: []
created_date: '2026-07-06 20:05'
updated_date: '2026-07-07 10:01'
labels:
  - go-caddy
  - 'crate:linux'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
VCLK-1 landed 2026-07-06. Inert MtClock plumbing, now_ns UNTOUCHED. shim.rs: MtClock(AtomicU64) newtype (tick/peek/advance_to/seed, Relaxed) + MT_CLOCK_TICK_NS=10us const (#[allow(dead_code)] until VCLK-2) next to CLOCK_* consts; mt_clock: Arc<MtClock> field on LinuxShim (Default zero via ..Self::default(); fresh Arc in fork ctor); mt_clock() pub(crate) accessor; seed(clock_ns) at the mt flip (alongside the still-authoritative clock_anchor). thread.rs: ThreadShared gains clock: Arc<MtClock>, ThreadShared::new(clock) takes it, run_threaded clones shim.mt_clock() before Arc-wrapping (no handle_mt sig change). 3 MtClock unit tests (tick=old+q + strict monotone reads, advance_to monotone-max, 8-thread concurrent never-decreases). DoD: full suite 283/284 (the 1 fail = go_http JIT leg flaking under concurrent-test load ~3.9 = pre-existing task-134 clock, passes 4.7s in isolation; VCLK-1 is inert so can't be cause); clippy -D warnings clean; fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
