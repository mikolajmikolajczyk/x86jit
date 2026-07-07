---
id: TASK-152
title: GP-5 — host-back the Flat path
status: Done
assignee: []
created_date: '2026-07-07 11:02'
updated_date: '2026-07-07 12:38'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
GP-5 done (0920143). x86jit-run non-Go Flat -> reserve_guarded host-backed; deep_copy Flat+Host arm region-copies (skip guards) -> Vec child so fork works; guarded_flat_in_span_load test; Vec residual pin dropped; decision-7+doc-30 residual updated. Full suite 250 green, clippy+fmt+perf gate clean. Guard pages GP-1..GP-5 complete.
<!-- SECTION:NOTES:END -->
