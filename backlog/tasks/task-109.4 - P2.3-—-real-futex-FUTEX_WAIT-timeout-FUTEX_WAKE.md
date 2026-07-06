---
id: TASK-109.4
title: 'P2.3 — real futex (FUTEX_WAIT + timeout, FUTEX_WAKE)'
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
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
