---
id: TASK-140
title: >-
  BGT-6 — background REGION tier-up (hotness-gated superblock formation) —
  deferred scope expansion
status: Done
assignee: []
created_date: '2026-07-06 18:24'
updated_date: '2026-07-07 14:57'
labels:
  - 'crate:core'
  - 'crate:cranelift'
  - 'goal:perf'
milestone: m-0
dependencies:
  - TASK-139
ordinal: 149000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Phase 6 of background-tier-plan.md (doc-27) — explicitly OUT of the v1 track (BGT-1..5 compile the single already-lifted IrBlock). This is the superblock-plan.md T3f 'future path to default-viability': region compile is too heavy inline even when hot (default-on regressed python 90s -> 280s), so form and compile superblocks only for proven-hot loops, in the background.

- Tier-up trigger (with a region-capable backend, Backend::region_caps Some) runs lift_region at hotness threshold instead of / in addition to the single-block submit; request carries the IrRegion (trait extension: a region-shaped TierUpRequest variant or a parallel method — design when picked up).
- Publish is a multi-span upgrade: TranslationCache::upgrade currently takes one (start,len) (cache.rs:116) — extend to a span list like insert already has, keeping the epoch-reject semantics and the spans-lock page-tag discipline (#12).
- Re-evaluate the superblock default-on decision (superblock-plan.md T3f) once regions only ever compile hot + off-thread.
Do not start before BGT-1..5 are Done and benched; re-read doc-27 and superblock-plan.md first.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Design note (task plan or doc update) settling the trait shape for region requests and the multi-span upgrade before code
- [ ] #2 Hot loop tiers up to a background-compiled region; interp == JIT on the full corpus with the mode on (env-gated)
- [ ] #3 Superblock default-on question re-measured and the outcome recorded (decision or task note)
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
DESIGN (AC#1), ratified 2026-07-07: UNIFIED ENUM + COEXIST.

Trait shape (core->backend, vm.rs):
- enum TierUpUnit { Block(Arc<IrBlock>), Region(Arc<IrRegion>) }.
- TierUpRequest: ir:Arc<IrBlock> -> unit:TierUpUnit; span:(u64,u32) -> spans:Vec<(u64,u32)>.
- TierUpFinished: span -> spans:Vec<(u64,u32)>.
- One tier_up_async path; the queue item IS the enum-carrier (no separate queue type).

Multi-span upgrade (cache.rs):
- Add upgrade_region(pc, block, spans:Vec, since_epoch, on_mark) = upgrade's epoch-reject + insert's multi-span store + page-tag (mark_code idempotent). drain_tier_up always uses it (a Block finish has spans.len()==1, pages already tagged -> idempotent). Keep single-span upgrade for the inline (non-bg) block path.

Cranelift (lib.rs):
- compile_request matches req.unit -> compile(block) | compile_region(region). worker pushes TierUpFinished{block:Compiled{entry}, spans:req.spans}.

Dispatcher (resolve, vm.rs):
- Gate the EAGER region path (currently vm.rs:863, fires on cache-miss/first-sight) behind !tier_up_background, so with_superblocks alone keeps eager inline regions (superblock.rs unchanged) but bg-on skips eager -> first sight lifts a single interpreted block.
- In the hotness bg path: after try_begin_tier_up(pc), if region_caps Some run lift_region(pc); a multi-block loop region -> Region request (spans=region.spans()); else -> Block request (existing). Region lift Err -> Block(ir) fallback.

Env gate: X86JIT_BG_TIER already gates bg; BGT-6 rides it + region_caps (with_superblocks). AC#3: re-measure superblock default-on (eager-inline vs hotness-bg regions) -> decision/task note. Tests: hot-loop forms a bg region, interp==JIT corpus with mode on.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE. AC#1 design note (unified enum + coexist) in plan. AC#2 hot_loop_tiers_up_to_a_background_region test + interp==JIT full corpus with X86JIT_BG_REGION on (13 x86jit-run integration + 8 Go incl net/http eager) + full suite 306 green. AC#3 outcome recorded in superblock-plan.md T3f: BGT-6 structurally removes the inline-region-cost objection (regions hotness-gated + off-thread), corpus-validated; default-flip deferred pending a clean-host region-corpus measurement (this box's noise floor per decision-9); mode ships env-gated off-by-default. DoD: nextest --features unicorn 306 green minus fuzz, clippy clean, fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
