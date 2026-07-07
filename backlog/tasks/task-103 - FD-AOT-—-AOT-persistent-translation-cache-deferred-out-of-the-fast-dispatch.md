---
id: TASK-103
title: FD-AOT — AOT / persistent translation cache (deferred out of the fast-dispatch
status: To Do
assignee: []
created_date: '2026-07-06 11:07'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:cranelift'
  - 'crate:core'
milestone: open-backlog
dependencies: []
ordinal: 103000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
AOT / persistent translation cache (deferred out of the fast-dispatch track, see [`../design/fast-dispatch-plan.md`](../design/fast-dispatch-plan.md) §D5). Attacks *compile* cost (the superblock M5-T3f amortization problem), orthogonal to the R1–R6 dispatch work. Structurally blocked today: compiled code bakes run-specific absolute addresses (link/IBTC slot heap addrs, helper fn addrs via `JITBuilder::symbol`, `is_pic=false`). Prereqs to record before starting: (1) slot-table indirection instead of baked slot addresses, (2) helper-table indirection, (3) `is_pic=true` + retained relocations, (4) cache key = guest-byte hash + lift/codegen version + consistency tier, (5) cross-run invalidation on key mismatch. Sequence only after the slot machinery it would serialize is stable (it is now, post-R6). **Execution-ready plan: [`../design/aot-plan.md`](../design/aot-plan.md)** (B0.1→B3, exact code sites) — supersedes prereq (3): `is_pic` stays `false`, relocatability via indirection (slot addrs are already `iconst`-baked; only the 6 helper calls emit relocs, so making them `call_indirect` leaves a reloc-free buffer). B0.1 (reloc-free codegen under JITModule) is the safe first commit.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
