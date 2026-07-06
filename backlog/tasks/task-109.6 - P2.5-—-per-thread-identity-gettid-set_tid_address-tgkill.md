---
id: TASK-109.6
title: 'P2.5 — per-thread identity: gettid / set_tid_address / tgkill'
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 115000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real per-thread tid (not ==pid); set_tid_address stores clear_tid; tgkill routing. Per-thread ctx passed into handle_mt; registry in ThreadShared. exit(60) ends one thread, exit_group(231) ends the process (distinction rides in the returned ThreadOp).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
