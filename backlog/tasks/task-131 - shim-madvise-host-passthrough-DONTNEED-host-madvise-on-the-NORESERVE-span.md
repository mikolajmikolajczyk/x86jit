---
id: TASK-131
title: >-
  shim: madvise host-passthrough (DONTNEED -> host madvise on the NORESERVE
  span)
status: In Progress
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-12 19:11'
labels:
  - 'crate:linux'
  - 'goal:feature'
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

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 shim test: madvise(DONTNEED) on a guest span reads back zeros afterwards (host passthrough observable)
<!-- AC:END -->
