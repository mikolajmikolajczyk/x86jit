---
id: TASK-217
title: >-
  watch: JIT-store dirty tracking misses stores when watch_count goes 0->nonzero
  mid-run on another vCPU
status: To Do
assignee: []
created_date: '2026-07-11 18:18'
labels:
  - memory
  - jit
dependencies: []
priority: medium
ordinal: 246000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found via unemups4 Fable phase-4 review (consumer of watch_range/take_dirty_ranges for its GPU resource-dirty tracking). The JIT store-watch gate is a PER-RUN SNAPSHOT: MemCtx::for_memory captures watch_count at run start (jit_abi.rs:265 'watch_count: mem.watch_count_snapshot()'), and generated code calls note_watched_write ONLY when that run's snapshot was non-zero (memory.rs note_watched_write path). Consequence: if a vCPU is mid-run in JIT'd code with a start-snapshot of 0 and ANOTHER thread calls watch_range() (0->nonzero transition), the running vCPU's stores to the newly-watched range go UNRECORDED until its next run boundary (next syscall/exit re-enters and re-snapshots). Per-page watch_page bits ARE checked live, so every watch installed while watch_count is already >0 is safe — the lossy window is exactly the 0->nonzero transitions (first watch ever, or first watch after a full unwatch). Single-threaded is safe (the watching thread's own snapshot refreshes on re-entry); multi-threaded guests are not. Interpreter stores unaffected (live gate). FIX DIRECTION: on a 0->nonzero watch_count transition, kick/refresh currently-running vCPUs so they re-snapshot (or make the JIT store path consult live watch state rather than gating on the start snapshot — measure against the task-204 zero-cost-when-unwatched goal). Repro shape: thread A runs a long JIT'd loop storing to range R (no watches at its run start); thread B watch_range(R) mid-loop; take_dirty_ranges() misses A's stores until A exits.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 a store executed by a JIT'd vCPU whose run started with watch_count==0, into a range watched (0->nonzero) by another thread mid-run, is reported by take_dirty_ranges before the storing vCPU next exits
- [ ] #2 no measurable cost added to the unwatched-store fast path (task-204 goal preserved); differential + existing watch tests stay green
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
