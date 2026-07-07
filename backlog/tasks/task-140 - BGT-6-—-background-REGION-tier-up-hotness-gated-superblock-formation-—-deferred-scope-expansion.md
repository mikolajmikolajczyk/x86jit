---
id: TASK-140
title: >-
  BGT-6 — background REGION tier-up (hotness-gated superblock formation) —
  deferred scope expansion
status: To Do
assignee: []
created_date: '2026-07-06 18:24'
updated_date: '2026-07-07 10:07'
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

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
