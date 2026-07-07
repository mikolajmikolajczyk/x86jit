---
id: TASK-152
title: GP-5 — host-back the Flat path
status: To Do
assignee: []
created_date: '2026-07-07 11:02'
labels:
  - go-caddy
  - 'crate:run'
  - 'goal:harden'
dependencies: []
ordinal: 161000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
doc-30 GP-5. x86jit-run non-Go Flat via reserve_guarded so every shim guest faults on wild in-span ptrs; drop residual Flat pin.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
