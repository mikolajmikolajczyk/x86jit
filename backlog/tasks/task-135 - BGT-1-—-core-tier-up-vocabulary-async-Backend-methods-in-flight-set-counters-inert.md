---
id: TASK-135
title: >-
  BGT-1 — core tier-up vocabulary: async Backend methods + in-flight set +
  counters (inert)
status: To Do
assignee: []
created_date: '2026-07-06 18:21'
updated_date: '2026-07-06 18:22'
labels: []
milestone: m-0
dependencies: []
ordinal: 144000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Phase 1 of background-tier-plan.md (doc-27, D1/D4). Core gains the vocabulary only — nothing calls it yet, zero behavior change, deps stay exactly {iced-x86} (spec §15).

- x86jit-core/src/vm.rs: TierUpRequest { pc, ir: Arc<IrBlock>, consistency, mmio, span, epoch }, TierUpFinished { pc, block, span, epoch }, enum TierUpSubmit { Queued, Busy, Unsupported }; trait Backend gains default-implemented tier_up_async(&self, req) -> TierUpSubmit (default Unsupported) and tier_up_finished(&self) -> Vec<TierUpFinished> (default empty, no alloc).
- x86jit-core/src/cache.rs: tier_pending: Mutex<HashSet<u64>> + try_begin_tier_up(pc) -> bool / end_tier_up(pc) (idempotent); invalidate_overlapping clears victims from tier_pending; counters tier_bg_published / tier_bg_rejected + accessors (fires-axis style, like chained/regions/ibtc_filled). Lock order: spans -> map -> hotness -> tier_pending.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Full test suite passes unchanged (nothing invokes the new API)
- [ ] #2 Cache unit tests cover every pending-set transition: begin/end, double-begin rejected, invalidate_overlapping clears victims from tier_pending, end_tier_up idempotent
- [ ] #3 x86jit-core Cargo.toml dependencies unchanged ({iced-x86} only)
- [ ] #4 clippy --all-targets --all-features clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
