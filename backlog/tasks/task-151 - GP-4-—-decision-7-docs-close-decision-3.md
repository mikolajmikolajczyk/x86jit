---
id: TASK-151
title: GP-4 — decision-7 + docs (close decision-3)
status: Done
assignee: []
created_date: '2026-07-07 11:02'
updated_date: '2026-07-07 12:21'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
GP-4 done. decision-7 created (accepted, supersedes decision-3). decision-3 status accepted->superseded + banner. jit.rs pin reframed as residual Vec-backed gap (unmapped_in_span_vec_backed_residual_gap); host-backed positive pinned in guard_pages.rs. AccessKind now PartialEq/Eq; load parity asserts access=Read. go-caddy-plan Phase-3 note (guard pages do half of fault->panic; delivery=task-123). Full suite 250 pass, clippy+fmt clean.
<!-- SECTION:NOTES:END -->
