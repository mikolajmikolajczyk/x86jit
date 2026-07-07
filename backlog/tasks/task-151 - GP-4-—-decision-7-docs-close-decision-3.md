---
id: TASK-151
title: GP-4 — decision-7 + docs (close decision-3)
status: To Do
assignee: []
created_date: '2026-07-07 11:02'
labels:
  - go-caddy
  - 'crate:none'
  - 'goal:harden'
dependencies: []
ordinal: 160000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
doc-30 GP-4. decision-7 supersedes decision-3; residual Vec pin; go-caddy Phase-3 note; close task-127. DoD nextest/clippy/fmt.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
