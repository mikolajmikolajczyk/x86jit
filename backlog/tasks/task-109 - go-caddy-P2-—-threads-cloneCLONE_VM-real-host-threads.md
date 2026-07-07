---
id: TASK-109
title: 'go-caddy P2 — threads: clone(CLONE_VM) -> real host threads'
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
milestone: go-caddy
dependencies: []
ordinal: 109000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Promote the proven mt.rs recipe into the production shim + a real driver. Arc<Mutex<LinuxShim>> over Arc<Vm>; a separate Arc<ThreadShared> (futex table + Condvar, exited flag, tid registry) for blocking state; yield-by-value (SyscallOutcome::ThreadOp) not pending_* on the shared shim (clobber trap); clone/futex/exit-vs-exit_group as ThreadOps; real clock in mt mode; fork+execve banned in a threaded process. Architect (Fable 5) reviewed the plan. HIGH risk, multi-session.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 pthreads.elf (4 threads x mutex -> 400000) runs through the production shim + driver, both engines
- [ ] #2 a threaded process cannot fork/execve (-EAGAIN / error, no host panic)
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
P2.0 Send foundation (done) -> ThreadShared -> threaded driver skeleton (whole single-process corpus on one worker thread, de-risk) -> futex -> clone -> identity -> clock -> DoD-1 -> fork/exec ban.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. go-caddy P2 threads complete: P2.0 Send refactor, P2.1 ThreadShared, P2.2 driver skeleton, P2.3 real futex, P2.4 clone(CLONE_VM) host-thread spawn, P2.5 per-thread identity + exit/exit_group split, P2.6 host-monotonic clock + interruptible sleep/yield, P2.7 pthreads.elf through production shim (both engines -> 400000), P2.8 fork/exec ban. Architecture per Fable-5 consult. Scope-expanders surfaced as task-121..125. Deferred: ARM/weak-host ordering (M7-T4).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
