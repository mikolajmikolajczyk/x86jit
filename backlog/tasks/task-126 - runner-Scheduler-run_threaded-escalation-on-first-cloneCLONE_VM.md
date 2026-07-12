---
id: TASK-126
title: 'runner: Scheduler->run_threaded escalation on first clone(CLONE_VM)'
status: Done
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-12 14:10'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 135000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope from P1b. The deferred Scheduler (proc.rs) must peek Rax==56 && Rdi&CLONE_VM BEFORE handle() services it with -ENOSYS, then hand its (Vm,Vcpu,LinuxShim) to a run_threaded entry that services the one pending clone before entering run_vcpu (RIP already advanced past the syscall). During the pre-escalation deferred phase of a Reserved VM, fork must answer -EAGAIN, never reach the core fork panic (memory.rs:296). Needed for shell-wraps-Go images (sh -c go-binary); NOT for the caddy image (execs directly). P1b instead selects threaded-vs-deferred up front per Go-note.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 runner test: single-threaded binary stays on the fast path; first clone(CLONE_VM) escalates and the program completes threaded
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (commit b1f0c2a + tid-fix follow-up). Deferred Scheduler now peeks is_clone_vm (Rax==56 && Rdi&CLONE_VM) before shim.handle(); RIP already advanced past the syscall (interp+jit), so run_process returns RunOutcome::Escalate(Box<Process>); Scheduler::run moves (vm,cpu,shim) into run_threaded_escalated, which services the pending clone via handle_mt (parent Rax=child tid, child Rax=0 — byte-identical to an in-loop first clone) before run_vcpu. Fast path = 2 reg reads + branch on the syscall exit only. fork stays -EAGAIN pre-escalation; a candidate with live pending/zombie children hard-errors (no orphaning; one-directional P2.8). Real pthreads.elf (4 pthreads via clone+futex) now runs through the deferred Scheduler on interp+jit. Code-review caught + fixed: spawn_child must reseed next_tid to child.pid+1 (fork seeded it to parent.pid+1 -> a forked child that escalates would collide its first thread tid with its own main-thread tid). clone3(435) deferred (musl/glibc pthreads use SYS_CLONE). KNOWN TEST GAP: the forked-child-escalates path (reap_pending Escalate arm) is fixed but not yet directly tested (escalate.rs covers root + real pthreads). 611/611, clippy+fmt clean.
<!-- SECTION:NOTES:END -->
