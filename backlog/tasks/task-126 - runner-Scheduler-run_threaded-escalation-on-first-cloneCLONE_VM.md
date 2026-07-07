---
id: TASK-126
title: 'runner: Scheduler->run_threaded escalation on first clone(CLONE_VM)'
status: To Do
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-07 10:08'
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
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
