---
id: TASK-159
title: >-
  bench: hotloop-length sweep validates adaptive tier selection picks the right
  tier
status: To Do
assignee: []
created_date: '2026-07-07 15:56'
updated_date: '2026-07-09 15:11'
labels:
  - 'crate:bench'
  - 'goal:test'
milestone: open-backlog
dependencies:
  - TASK-156
ordinal: 168000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Validation harness for task-156's adaptive tiering: run the hotloop workload at a SWEEP of iteration counts (short -> long) under adaptive mode and assert the dispatcher picks single-block-bg for short loops (no region formed, no regression) and climbs to a region for long loops (region formed, ~2x win). Proves the per-block policy self-selects the right tier without a manual mode switch — the whole point of the production tiered model.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A sweep (e.g. loop N in {1k, 10k, 100k, 1M, 10M}) shows: below the region backedge threshold no region forms (cache.regions()==0, timing tracks single-block bg); above it a region forms and the ~2x warm-loop win appears
- [ ] #2 The crossover is stable and documented (a table in the experiment output or a note)
- [ ] #3 bench asserts (not just prints): for each hotloop length band the selected tier matches the expected one
<!-- AC:END -->



## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
guest_hotloop already takes an iters param — parametrize the experiment/a new subcommand over N. Assert cache.regions() and compare region-bg timing vs bg per N. This is the empirical proof that adaptive tiering (task-156) beats a static global mode. Builds on the bench region-bg column (task-140) + guest_hotloop already committed.
<!-- SECTION:PLAN:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
