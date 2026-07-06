---
id: TASK-109.4
title: 'P2.3 — real futex (FUTEX_WAIT + timeout, FUTEX_WAKE)'
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-06 12:42'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 113000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Port mt.rs futex (per-address generation + Condvar). Value re-check lives INSIDE futex_wait under the futex mutex (linearization point). WAIT needs wait_timeout (Go futexsleep). WAKE non-blocking, can complete inline in handle under the lock-order rule.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. SyscallOutcome enum + LinuxShim::handle_mt in shim.rs (futex intercepted by value; everything else routes through handle, yield-bool→Continue/ProcessExit/Unsupported). ThreadShared::futex_wait/futex_wake in thread.rs (per-address generation + Condvar, value re-check under futex mutex = linearization point, relative timeout via wait_timeout w/ overflow guard, 50ms poll backstop, exit releases waiters). Driver wired: FUTEX_WAIT/WAKE serviced after shim guard drops (lock order shim→futex). 4 unit tests (eagain/timeout/wake/exit-release) + threaded_driver corpus (4/4 through handle_mt) + full suite 198/198. clippy/fmt clean. Next: P2.4 clone(CLONE_VM)→spawn host thread (SyscallOutcome::Spawn variant).
<!-- SECTION:NOTES:END -->
