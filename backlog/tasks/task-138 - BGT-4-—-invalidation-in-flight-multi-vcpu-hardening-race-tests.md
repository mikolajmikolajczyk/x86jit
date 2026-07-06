---
id: TASK-138
title: BGT-4 — invalidation-in-flight + multi-vcpu hardening (race tests)
status: To Do
assignee: []
created_date: '2026-07-06 18:23'
labels: []
milestone: m-0
dependencies:
  - TASK-137
ordinal: 147000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Phase 4 of background-tier-plan.md (doc-27, D5). The correctness net around a compile racing invalidation — sequenced deterministically with wait_idle, no sleeps. The epoch machinery (cache.upgrade rejecting on a moved epoch, the #3 race tests at cache.rs:291-330) already carries the load; these tests prove the new, wider submit->publish window and fix whatever they force.

- SMC write to the hot block's page while its compile is queued/in flight: publish rejected (epoch moved), tier_pending cleared by invalidate_overlapping, block re-lifts, re-heats, re-tiers successfully.
- Trap-region Vm::map mid-flight (full flush + epoch bump, vm.rs:198-204): stale compile rejected; block resubmits with the new mmio window baked.
- Unrelated invalidation (epoch bump, block NOT a victim): publish rejected, end_tier_up in the drain lets it resubmit; second attempt publishes.
- Duplicate completions for one pc (invalidate + re-heat while the old request is still queued): epoch check picks the right one regardless of drain order.
- Threaded driver (x86jit-linux/src/thread.rs, shared Arc<Vm>) with bg on: multi-vcpu drain — each completion published exactly once, output equality vs interp; run under the mt test harness.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 All five race scenarios above have deterministic tests and pass
- [ ] #2 tier_bg_rejected counter observed firing in the rejection tests; tier_pending provably empty at each test end (no stuck in-flight marker)
- [ ] #3 mt/threaded suite green with bg on (locally exercised; ARM leg via the manual CI workflow)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
