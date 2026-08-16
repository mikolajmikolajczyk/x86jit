---
id: TASK-350
title: >-
  core: unwatch can drop a bitmap word from the drain scan-list that a
  concurrent watch just re-armed
status: To Do
assignee: []
created_date: '2026-08-15 13:28'
labels:
  - code-review
dependencies: []
priority: high
ordinal: 386000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`WatchPages::unwatch` (memory.rs:471-473) clears the bit, re-reads the word, and if it is now zero removes the word index from `words`. A concurrent `watch()` of a DIFFERENT page in the same word does fetch_or + insert into `words` inside that window; the racing unwatch then removes the index again. `take_dirty` only scans `words`, so every dirty bit in that word is lost.

Probe on an x86 host -- a pure logic race, not weak memory -- with a negative control. Two threads on one Memory: one churns watch_range/unwatch_range on page 1; the other loops watch_range(page2); note_write(page2); take_dirty_ranges(); unwatch_range(page2) and counts drains that failed to report its own write.

    control (a third page in the same word watched permanently, so word.load()==0 never true):
                                              0 lost / 2000000
    word churns through zero:              3736 lost / 2000000

The control isolates lines 471-473 as the cause.

Guest-side is unaffected (the SMC path uses a separate table). The embedder sees silent corruption: a written guest page is never reported dirty, so an embedder caching guest-backed resources -- the documented use is GPU buffers -- serves stale bytes forever.

In contract: Vm::watch_range/unwatch_range take &self and Vm is Sync, so concurrent registration is explicitly permitted, and pages within 256 KiB share a word. The documented workload is 'a few small textures/index buffers scattered across a 41 GiB heap'.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A word is removed from the scan-list only if no page in it is watched at the moment of removal
- [ ] #2 The racing test above is in the suite and fails against the current code
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
