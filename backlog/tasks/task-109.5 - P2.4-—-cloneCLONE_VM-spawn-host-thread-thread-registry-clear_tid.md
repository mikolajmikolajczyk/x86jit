---
id: TASK-109.5
title: P2.4 — clone(CLONE_VM) -> spawn host thread + thread registry + clear_tid
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 114000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
clone arm builds child CpuState (RAX=0, RSP, CLONE_SETTLS->FsBase, PARENT/CHILD_SETTID writes, CHILD_CLEARTID recorded) and yields ThreadOp::Spawn; the driver (owns all three Arcs) spawns the vcpu loop. On thread exit: write 0 to clear_tid + futex_wake.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
