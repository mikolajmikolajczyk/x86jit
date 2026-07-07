---
id: TASK-137
title: >-
  BGT-3 — dispatcher wiring: hot-path enqueue + drain/publish in resolve, opt-in
  flag
status: Done
assignee: []
created_date: '2026-07-06 18:23'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
  - 'crate:tests'
milestone: m-0
dependencies:
  - TASK-135
  - TASK-136
ordinal: 146000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Phase 3 of background-tier-plan.md (doc-27, D2/D4/D6) — the feature lands, opt-in, default off (same stance as task-106: the differential/fuzz corpus must not depend on when the interp->compiled switch happens).

- x86jit-core/src/vm.rs resolve (top, ~673): when tier_up_background, drain backend.tier_up_finished() and publish each via the existing cache.upgrade(pc, block, span, epoch) (cache.rs:116); ALWAYS end_tier_up(pc) after the publish attempt (success or reject); bump tier_bg_published/rejected.
- resolve hot path (~688-699): when tier_up_background and bump_hotness >= thr, try_begin_tier_up(pc) then tier_up_async with the epoch snapshot already taken at ~679; Queued -> keep interpreting; Busy -> end_tier_up (retry via hotness later); Unsupported -> end_tier_up + fall through to today's inline materialize+upgrade. Never compile inline on Busy (that would reintroduce the spike under peak compile pressure).
- Vm::set_tier_up_background(bool), field beside tier_up_after (vm.rs:126), inherited by fork_with_backend; only meaningful with tier_up_after Some + an async-capable backend.
- x86jit-tests/src/guest.rs builder: .tier_up_background() beside .tier_up() (guest.rs:150), wire in the vm setup (~225); expose the TierUpHandle for tests.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Deterministic tier-up test (doc-27 D6 recipe): low threshold + bg on, run hot block >= thr times (assert still interpreted/pending), wait_idle, one more dispatch publishes; tier_bg_published == 1 and final state equals the interpreter oracle — no sleeps or timing
- [ ] #2 Real-program run with bg on: output byte-identical to interp and tier_bg_published > 0
- [ ] #3 Env-gated X86JIT_BG_TIER=1 differential sweep green (mirrors the X86JIT_SUPERBLOCKS=1 precedent)
- [ ] #4 Default-off suite untouched: full corpus + fuzz configs unchanged and green
- [ ] #5 InterpreterBackend (Unsupported fallback) with bg flag on behaves exactly like today's inline tier-up
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
BGT-3 landed 2026-07-06. Feature lands, opt-in default-off. vm.rs: tier_up_background field + set_tier_up_background (inherited by fork_with_backend). resolve() top drains backend.tier_up_finished() and publishes each via existing cache.upgrade (drain_tier_up helper: always end_tier_up, bump tier_bg_published/rejected). resolve hot path: bg on + hot -> try_begin_tier_up + tier_up_async(epoch snapshot); Queued->keep interp; Busy->end_tier_up+keep interp (no inline spike); Unsupported->end_tier_up+inline fallback; already-pending->keep interp. guest.rs builder .tier_up_background(). oracle.rs run_with_backend: X86JIT_BG_TIER=1 env sweep (tier 2 + bg). 3 integration tests (bg_tier.rs): deterministic (published 0->0->1 via wait_idle, no sleeps), real loop (bg==interp + published>0), interp Unsupported fallback==inline. DoD: nextest --features unicorn 276/276 green minus fuzz; X86JIT_BG_TIER=1 jit sweep 45/45; clippy clean; fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
