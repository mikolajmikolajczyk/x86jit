---
id: TASK-109
title: 'go-caddy P2 — threads: clone(CLONE_VM) -> real host threads'
status: In Progress
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-06 11:15'
labels: []
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

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
