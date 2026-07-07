---
id: TASK-123
title: 'signals: per-thread tgkill delivery (registry + guest signal frames)'
status: To Do
assignee: []
created_date: '2026-07-06 12:51'
updated_date: '2026-07-07 10:08'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 132000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope split from P2.5. The fatal-abort path (128+sig, whole-process exit) covers abort(); real per-thread signal routing needs a tid->thread registry + guest signal-frame synthesis. Signal-machinery milestone, not P2.5. Blocked-on: signal frame synthesis.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
