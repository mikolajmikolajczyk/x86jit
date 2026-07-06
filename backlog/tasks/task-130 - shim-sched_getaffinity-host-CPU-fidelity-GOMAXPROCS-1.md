---
id: TASK-130
title: 'shim: sched_getaffinity host-CPU fidelity (GOMAXPROCS>1)'
status: To Do
assignee: []
created_date: '2026-07-06 13:40'
labels: []
milestone: go-caddy
dependencies: []
ordinal: 139000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope; PERF phase. Current arm returns a 1-CPU mask -> Go GOMAXPROCS=1 (deterministic, correct ABI, one P — right for bring-up). Host-CPU fidelity gives GOMAXPROCS>1 but introduces scheduling nondeterminism; defer deliberately until perf phase.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
