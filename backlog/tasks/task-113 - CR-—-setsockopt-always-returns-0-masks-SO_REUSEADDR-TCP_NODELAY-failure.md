---
id: TASK-113
title: 'CR — setsockopt always returns 0, masks SO_REUSEADDR/TCP_NODELAY failure'
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-07 10:07'
labels:
  - 'crate:linux'
  - 'goal:fix'
milestone: code-review
dependencies: []
ordinal: 122000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
shim.rs SYS_SETSOCKOPT reports success even when the host rejects an option. Deliberate today (guests treat most as advisory) but a guest checking the return can't detect failure.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
