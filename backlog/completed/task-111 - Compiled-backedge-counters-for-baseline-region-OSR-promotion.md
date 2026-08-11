---
id: TASK-111
title: Compiled backedge counters for baseline->region OSR promotion
status: Done
assignee: []
created_date: '2026-07-07 16:17'
updated_date: '2026-08-11 11:03'
labels:
  - 'crate:core'
  - 'crate:cranelift'
  - 'goal:perf'
milestone: ps4-perf
dependencies:
  - TASK-107
ordinal: 169000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The real adaptive-tiering win task-107 uncovered: a hot loop must baseline-compile at T1 (single block, so it gets a speedup immediately) and PROMOTE to a superblock region only at a much higher backedge threshold T2 — production OSR (HotSpot backedge counter, V8/JSC OSR). task-107 shipped the two-threshold plumbing but its dispatcher counter can't drive this: once a block single-block-compiles at T1 and CHAINS (link slots), it never returns to the dispatcher, so a dispatcher-side T2 counter never fires (measured: sha256 either regions-and-regresses or, if kept out via a high T2, stays interpreted -> 0.1x). The counter must live IN the compiled code. Emit a back-edge counter in the compiled loop block: increment on each back-edge, and when it crosses T2 trap out (a new RET_ code) so the dispatcher submits a background region for that pc. Then a candidate loop runs baseline-compiled (fast) T1..T2 and promotes to a region once genuinely long-running — no interpret-until-T2 footgun, and short/one-shot hot loops keep the baseline tier (no region regression).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A hot loop baseline-compiles at T1 (single block) and promotes to a background region at T2 via an in-code backedge counter; interp==JIT on the full corpus
- [ ] #2 bench: with the corpus wired to adaptive region-bg, sha256/sqlite/lua/go do NOT regress (they keep the baseline tier) AND hotloop still wins ~2x — the full picture task-107 could not deliver
- [ ] #3 x86jit-run X86JIT_BG_REGION uses adaptive T2 safely (no hot-but-short loop stuck interpreted)
- [ ] #4 test: a loop hot enough for OSR promotes mid-execution and the result matches interp (jit_eq_interp on a long-running loop)
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Codegen: in a region/loop block, emit an AtomicU32 backedge counter (a baked slot like the link slots) incremented at the back-edge; a compare against T2 + brif to a new trap-out (RET_REGION_PROMOTE) carrying pc. Dispatcher: on RET_REGION_PROMOTE, if region_decision(pc)==candidate and !already-regioned, lift_region + submit a Region request (the BGT-6 path). Reuse task-107's region_decision map + tier_up_region_after (T2) + the multi-span upgrade. Then wire the bench region-bg (TierCfg::region_after, already added) + x86jit-run to a sane T2 and re-measure. Note the hot-path cost of the counter (one atomic inc per back-edge) — measure it; keep it behind region_caps. Refs: superblock-plan.md T3f, decision-7, task-100 (BGT-6), x86jit-cranelift codegen ret codes.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Merged into the performance roadmap task 2026-08-11. Kept together because they share one precondition: task-216 attributed the cost, and task-217 showed four locally-good micro-optimisations moving the real workload by zero. Individually they invite work that measures well and changes nothing.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
