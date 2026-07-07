---
id: TASK-131
title: >-
  shim: madvise host-passthrough (DONTNEED -> host madvise on the NORESERVE
  span)
status: To Do
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
milestone: go-caddy
dependencies: []
ordinal: 140000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope; LOW priority (footprint). P3 lands madvise->0 no-op (correct: advisory, Go does not rely on advice-zeroing). A host-passthrough (guest range -> base+addr madvise on the NORESERVE span) reclaims RSS for a long-running caddy. Footprint optimization, not correctness.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
