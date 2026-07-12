---
id: TASK-124
title: 'mm: reclaim thread stacks (munmap-aware mmap accounting)'
status: Done
assignee: []
created_date: '2026-07-06 12:51'
updated_date: '2026-07-12 19:50'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 133000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope split from P2.4. The mmap bump allocator never reclaims; a thread-churning server leaks guest address space. Irrelevant for bounded-thread acceptance programs (pthreads.elf). Task: munmap-aware mmap accounting.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 mm test: spawn+join N threads in a loop — mmap accounting shrinks after joins (no monotonic growth assert)
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged 9a57fdb + doc 2eb502b). Anonymous mmap arena now reclaims munmap'd spans: mmap_live/mmap_free BTreeMaps + mmap_high water-mark; arena_alloc first-fits the free list then bumps; arena_free splits partial unmaps + release_span rolls back top-of-bump (cascading) or coalesces into the free list. Reused spans re-zeroed (dirty-water-mark gates bump-reuse zeroing — agent caught+fixed a python heap-corruption from unzeroed rollback). Fixes the thread-churn arena leak (pthread stacks reclaimed). MAP_FIXED/file-backed/mprotect excluded; fork clones the maps. Review: zero-guarantee + accounting + fork + concurrency all clean. Review's HIGH 'stale JIT code on address reuse -> interp!=JIT' = FALSE POSITIVE: zero_span's write_bytes runs note_write (embedder-write SMC path, memory.rs:770) -> handle_smc (every dispatch) drops the stale block; covered by smc.rs::embedder_rewrite_reexecutes_{interp,jit}. Documented at arena_alloc. 11 arena tests + full suite green.
<!-- SECTION:NOTES:END -->
