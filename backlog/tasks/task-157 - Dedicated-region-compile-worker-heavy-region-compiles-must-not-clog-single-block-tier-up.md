---
id: TASK-157
title: >-
  Dedicated region compile worker (heavy region compiles must not clog
  single-block tier-up)
status: To Do
assignee: []
created_date: '2026-07-07 15:55'
updated_date: '2026-07-09 15:11'
labels:
  - 'crate:cranelift'
  - 'goal:perf'
milestone: open-backlog
dependencies:
  - TASK-140
ordinal: 166000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measured BGT-6 bottleneck (task-140 AC#3, superblock-plan.md T3f): the SINGLE background compiler worker serializes region compiles (far heavier than a block's) behind single-block tier-ups, so hot code stays interpreted longer -> part of why region-bg regressed one-shot workloads. Production JITs run separate compile queues/threads per tier (HotSpot C1 vs C2 compiler threads). Give the region tier its own worker/queue so a slow region compile can't stall the cheap single-block tier-up path.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Region and single-block compiles run on separate workers/queues; a long region compile does not delay single-block tier-up publication
- [ ] #2 bench: region-bg regression on mixed workloads shrinks vs the single-worker baseline (task-140 numbers)
- [ ] #3 test: a heavy region compile queued behind hot single-block tier-ups does not delay them (latency asserted or bounded)
<!-- AC:END -->



## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
x86jit-cranelift Shared: today one queue + one worker_loop (lib.rs). Add a second worker (or a priority queue: single-block requests jump ahead of region requests). TierUpUnit already distinguishes Block vs Region, so the split is natural. Keep the done/ready drain unified (core drain_tier_up is agnostic). Watch thread count on many-core vs few-core hosts; gate the extra thread on region_caps being Some. Pairs with task-156 (adaptive thresholds) — together they make region tiering pay without starving the baseline tier.
<!-- SECTION:PLAN:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
