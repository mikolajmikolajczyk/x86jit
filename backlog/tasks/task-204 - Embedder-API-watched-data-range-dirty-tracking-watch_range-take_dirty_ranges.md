---
id: TASK-204
title: >-
  Embedder API: watched-data-range dirty tracking (watch_range /
  take_dirty_ranges)
status: To Do
assignee: []
created_date: '2026-07-10 19:04'
labels:
  - perf
dependencies: []
ordinal: 233000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Requested by the unemups4 embedder (its GPU resource cache — see unemups4 doc-4 §8.3 / decision-4). unemups4 needs to know when the GUEST writes a data page backing a cached GPU resource (texture / vertex / index / constant buffer / shader binary / render target), so it can invalidate + re-upload lazily. Current state (x86jit-core/src/memory.rs): dirty tracking exists but is CODE-ONLY — note_write records a dirtied page ONLY if that page was previously mark_code'd (SMC support), and take_dirty_code() drains that code-page set. A non-code data page the guest writes records nothing. So take_dirty_code cannot serve as a GPU-cache dirty source. NEEDED: a PARALLEL facility for embedder-registered DATA ranges, independent of the code-page mechanism (can share the note_write hot-path check): register a set of watched guest address ranges, and drain those written since the last poll. Sketch API (embedder-facing, on Vm or a handle): watch_range(addr: u64, size: u64) / unwatch_range(...); take_dirty_ranges() -> Vec<(u64,u64)> (or a page/bitset form) draining writes since last call. Poll-and-drain at frame/submit boundaries is the intended usage, so it must NOT require ordering guarantees beyond what MemConsistency::Fast provides (unemups4 runs Fast). Zero cost when no ranges are watched (one predictable branch on the write path). Not urgent for unemups4 phase 3.5 (it uses conservative per-submit re-upload), but required before phase 4's texture cache; filing now so it can land + be rev-pinned ahead of need.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Embedder can register/unregister watched guest data ranges (watch_range/unwatch_range)
- [ ] #2 take_dirty_ranges() (or equivalent) returns and drains the watched ranges/pages written since the previous call; independent of the code-page mark_code/take_dirty_code path
- [ ] #3 Zero measurable overhead on the write path when no ranges are watched; differential corpus green
- [ ] #4 Works under MemConsistency::Fast (poll-and-drain at submit/frame boundaries, no extra ordering requirement); unit-tested
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
