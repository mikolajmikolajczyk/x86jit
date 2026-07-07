---
id: TASK-117
title: 'CR — lock bts/btr/btc [mem],reg lifts to a non-atomic byte RMW'
status: To Do
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-07-07 10:02'
labels:
  - 'crate:core'
milestone: code-review
dependencies: []
ordinal: 126000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
lift.rs mem-BT has no has_lock_prefix -> AtomicRmw path (matches the immediate-form gap). Concurrent lock bit-ops on a shared bitmap can tear. Pre-existing; relevant once P2 threads land.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
