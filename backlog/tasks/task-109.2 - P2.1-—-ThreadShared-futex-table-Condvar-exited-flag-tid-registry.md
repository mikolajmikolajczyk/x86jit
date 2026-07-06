---
id: TASK-109.2
title: 'P2.1 — ThreadShared: futex table + Condvar, exited flag, tid registry'
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 111000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Separate Arc (NOT under the shim mutex) holding: futex Mutex<HashMap<u64,u64>> + Condvar, exited AtomicBool + exit_code, next_tid, thread registry (tid -> JoinHandle + clear_tid). Lock-order rule: shim->futex allowed, never block on futex_cv holding the shim guard.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
