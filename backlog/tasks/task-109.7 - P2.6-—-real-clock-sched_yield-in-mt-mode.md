---
id: TASK-109.7
title: P2.6 — real clock + sched_yield in mt mode
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 116000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Switch clock_gettime/nanosleep to host CLOCK_MONOTONIC + real sleep when threads exist (virtual tick clock keeps the single-threaded corpus deterministic; record the determinism-loss decision). sched_yield -> yield_now, 0.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
