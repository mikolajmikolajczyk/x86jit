---
id: TASK-126
title: 'runner: Scheduler->run_threaded escalation on first clone(CLONE_VM)'
status: Done
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-12 13:57'
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
Done 2026-07-12. Escalation implemented: run_process returns RunOutcome{Exited|Escalate}; peeks Rax==56 && Rdi&CLONE_VM before handle() (RIP already past syscall via core), hands (vm,cpu,shim) to new thread::run_threaded_escalated which services the one pending clone via handle_mt->clone_thread->Spawn (parent Rax=child_tid, threaded flip+clock seed), then run_vcpu. fork stays -EAGAIN pre-escalation. Fast path: is_clone_vm() = 2 reg reads + branch on syscall exit only. Tests: x86jit-tests/tests/escalate.rs (hand-asm clone escalates, fork stays deferred, + real pthreads.elf 400000 via Scheduler, interp+jit). Suite 609 green, clippy+fmt clean. clone3(435) not needed (glibc/musl pthreads emit SYS_CLONE).
<!-- SECTION:NOTES:END -->
