---
id: TASK-122
title: 'futex: robust list support (set_robust_list is a deliberate no-op)'
status: To Do
assignee: []
created_date: '2026-07-06 12:51'
updated_date: '2026-07-07 10:08'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 131000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope. set_robust_list no-op-d today; matters for pthread_mutex_robust / dying-thread lock recovery (Go runtime uses it defensively). Document the no-op as deliberate; revisit when a real guest misbehaves.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
