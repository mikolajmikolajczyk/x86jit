---
id: TASK-306
title: Link/IBTC publication is not validated against the invalidation epoch
status: Done
assignee: []
created_date: '2026-08-10 15:38'
updated_date: '2026-08-11 11:02'
labels: []
dependencies: []
priority: high
ordinal: 342000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Reported independently by two adversarial reviews reading from opposite ends — vm.rs (~1205, RET_LINK resolves and clones a CompiledPtr, then publishes without rechecking the epoch) and codegen/mod.rs (~3690, compiled code loads a nonzero link entry and returns it for chaining with no validation; ibtc_or_miss ~3717 and return prediction ~3832 have the same window).

Two independent passes converging on one mechanism is why this is filed high rather than as a theoretical race. Another vcpu can invalidate and clear every slot between resolve and publish; this side then republishes the stale pointer and jumps to it. Keeping code bytes allocated prevents use-after-free, which is why it has never crashed — it does not prevent executing a translation of code the guest has since rewritten.

Needs a deterministic test that pauses between slot load and transfer.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Entries carry an epoch validated immediately before transfer, or publication is serialised with invalidation
- [ ] #2 Direct links, IBTC and return prediction are all covered
- [ ] #3 A two-vcpu test that pauses after the slot load and invalidates proves the stale translation cannot run
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Merged into the multi-vcpu soundness task 2026-08-11. Same property, same test harness (a deterministic two-vcpu race), and fixing any one of them in isolation leaves the guarantee unstated. Each site kept its own acceptance criterion there.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
