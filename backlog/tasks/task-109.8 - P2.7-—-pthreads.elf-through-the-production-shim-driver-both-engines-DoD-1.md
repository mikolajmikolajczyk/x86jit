---
id: TASK-109.8
title: 'P2.7 — pthreads.elf through the production shim + driver, both engines (DoD-1)'
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 117000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Wire the acceptance program through the real shim (not the mt.rs toy handle). Replace/augment the mt.rs test with a shim-driven one. Closes M7-T5 properly.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
