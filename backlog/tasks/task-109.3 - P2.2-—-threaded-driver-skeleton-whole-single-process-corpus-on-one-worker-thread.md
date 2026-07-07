---
id: TASK-109.3
title: >-
  P2.2 — threaded driver skeleton: whole single-process corpus on one worker
  thread
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
  - 'crate:tests'
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 112000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
New x86jit-linux/src/thread.rs driver that ADOPTS a mid-flight process: consume owned (Vm,Vcpu,LinuxShim), return ProcOutcome/ProcError, take initial_op. De-risk step (Fable): run every existing single-process test through it on ONE thread before any concurrency, validating Send + &Vm + the yield protocol shape.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
run_threaded skeleton: adopts (Vm,Vcpu,LinuxShim), Arc<Mutex<shim>> over Arc<Vm>, one worker vcpu loop locking the shim per syscall. De-risk test tests/threaded_driver.rs: hello_static/musl/newton/sha256 through the driver == inline loop, both backends. pending_* yield → typed error not panic (partial P2.8).
<!-- SECTION:NOTES:END -->
