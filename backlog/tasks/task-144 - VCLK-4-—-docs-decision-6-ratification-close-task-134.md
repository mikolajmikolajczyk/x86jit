---
id: TASK-144
title: 'VCLK-4 — docs + decision-6 ratification, close task-134'
status: To Do
assignee: []
created_date: '2026-07-06 20:06'
labels:
  - go-caddy
dependencies:
  - TASK-143
ordinal: 153000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Wrap-up (threaded-clock-plan.md VCLK-4). Maintainer flips decision-6 proposed->accepted and edits decision-4's status line to 'Superseded by decision-6 (clock value domain; real blocking, single-threaded preservation, and the non-assertion rule carry forward)'. Update backlog/docs/status.md (threaded clock now virtual-monotonic); deferred.md entries from plan M6 (SYS_POLL stays time-free; single clock domain; no vDSO; no host-time governor; blocking host-fd I/O uncredited); architecture.md/glossary.md if they mention the mt clock. Resolve open decision 3 (tier-up dodge fate) and close task-134.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 decision-6 accepted; decision-4 status back-linked
- [ ] #2 status.md + deferred.md updated; task-134 Done
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
