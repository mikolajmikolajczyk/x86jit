---
id: TASK-156
title: 'Adaptive per-block tiering: backedge-gated region threshold (T2 >> T1)'
status: To Do
assignee: []
created_date: '2026-07-07 15:55'
labels:
  - 'crate:core'
  - 'goal:perf'
milestone: open-backlog
dependencies:
  - TASK-140
ordinal: 165000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Make tier selection PER-BLOCK and self-adjusting, the way production JITs do (HotSpot C1/C2, V8 tiers, JSC), instead of one global env-gated mode. Today BGT-6 forms a region at the SAME hotness threshold as a single block (resolve: bump_hotness>=thr -> lift_region), so a short 'loop' region-compiles before it can amortize -> the measured one-shot regression (superblock-plan.md T3f, decision-9 sibling). Fix: TWO thresholds. T1 (low, ~50) -> background single-block compile (cheap, helps most, ~C1/baseline). T2 (much higher, a BACKEDGE count, ~5000+) -> background REGION (~C2/OSR). A loop iterating 100x stays single-block-bg; one iterating 100k+ climbs to a region. Automatically avoids the one-shot regression AND captures the hotloop 2x win — no env var, no global choice; each block finds its own tier.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Region tier-up is gated behind a separate, higher backedge/hotness threshold T2 (not T1); single blocks tier at T1 as today
- [ ] #2 bench experiment: a SHORT hot loop stays single-block-bg (no region regression), a LONG hot loop tiers up to a region (~2x win) — selected automatically, no mode switch
- [ ] #3 interp == JIT on the full corpus with adaptive tiering on; hotloop still wins, one-shot/sha256 no longer regress
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Signals: a loop header is a block with a back-edge (lift_region would yield a multi-block loop). Count back-edge traversals (or reuse bump_hotness but only ARM the region path once a separate region counter crosses T2). Keep single-block bg at T1. Refs: x86jit-core/src/vm.rs resolve() hotness path (region lift currently at thr); cache.bump_hotness; TierCfg. Wire a Vm knob (tier_up_region_after: Option<u32>) beside tier_up_after; x86jit-run gate it (X86JIT_BG_REGION implies a sane T2). This is the OSR analogue — a long-running loop never returns, so a backedge counter is what detects it. Builds directly on BGT-6 (region request + multi-span upgrade already exist).
<!-- SECTION:PLAN:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
