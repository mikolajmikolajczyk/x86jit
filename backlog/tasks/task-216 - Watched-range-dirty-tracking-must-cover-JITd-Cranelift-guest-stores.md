---
id: TASK-216
title: Watched-range dirty tracking must cover JIT'd (Cranelift) guest stores
status: To Do
assignee: []
created_date: '2026-07-11 16:12'
labels:
  - perf
dependencies: []
ordinal: 245000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-204 added embedder watched-data-range dirty tracking (watch_range/unwatch_range/take_dirty_ranges) but the watch check lives ONLY in Memory::note_write (x86jit-core/src/memory.rs:454-478), which fires solely on interpreter-executed stores + embedder write_bytes. The Cranelift JIT INLINES guest stores as raw host stores with NO watch/SMC check: emit_store → checked_addr (bounds+MMIO only) → store_guest → plain gstore (x86jit-cranelift/src/codegen/memory.rs:12-18, codegen/mod.rs ~1821-1958); the rep-movs string helper uses the same bounds-only view. The JIT's own comment says it (x86jit-cranelift/src/lib.rs:87 'inlined stores skip SMC/region handling, deferred §10'). CONSEQUENCE for the unemups4 embedder: it runs the JIT by default with tier-up after ~50 execs, so any hot loop writing dynamic vertex/constant/texture data — exactly the workload its GPU resource cache invalidates on — becomes INVISIBLE to take_dirty_ranges the moment it tiers up. Confirmed absent at both the currently-pinned rev (26bc5ec) and HEAD (47b7e6f): cranelift codegen has no watch handling; task-204's ACs never exercised JIT'd stores. This blocks unemups4 phase-4 task-48 (its AC #2 = 'JIT'd guest code writing a watched range → take_dirty_ranges reports it' cannot pass) and downstream cache correctness (silently stale GPU buffers/textures). Requested by the unemups4 embedder (GPU resource cache, doc-4 §8.3).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A guest store executed by the CRANELIFT JIT into a watched range is reported by take_dirty_ranges (not just interpreter/write_bytes stores) — regression test that runs the same store under both interp and JIT and asserts identical dirty output
- [ ] #2 String/block helpers (rep movs and any bulk-store path) that write a watched range are covered too
- [ ] #3 Zero measurable overhead on the JIT store path when no ranges are watched (watch_count-gated inline check, mirroring the existing note_write one-relaxed-load gate)
- [ ] #4 Unicorn differential corpus green; interp-vs-JIT parity on dirty output; clippy -D warnings + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
