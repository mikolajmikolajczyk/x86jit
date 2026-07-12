---
id: TASK-234
title: >-
  madvise host-passthrough bypasses note_write (watchpoint dirty-tracking +
  interp SMC)
status: To Do
assignee: []
created_date: '2026-07-12 19:39'
labels:
  - 'crate:linux'
  - 'goal:fidelity'
dependencies: []
ordinal: 263000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-131 review findings #2/#3 (Low, latent). madvise_dontneed's host libc::madvise zeroes the inner pages via a raw pointer, bypassing Memory::note_write. Consequences: (#2) task-204 watchpoint dirty-tracking (watch_range/take_dirty_ranges) does not record the inner pages' change to zero — latent, no in-repo production embedder calls watch_range (tests only), but a CRIU/incremental-snapshot embedder would miss it. (#3) interpreter SMC: a guest that MADV_DONTNEEDs a page it also executes translated code from won't get the interp block invalidated (JIT unaffected — JIT-side SMC already deferred, vm.rs:519-523). Edge slivers (routed through zero_range->write_bytes->note_write) ARE recorded; only inner full pages are missed. Fix: route the madvise'd range through note_write/note_watched_write (or note the dirty range) when watch_count>0 / for code pages.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 a MADV_DONTNEED over a watched page records the change in take_dirty_ranges; a DONTNEED over an interp-translated code page invalidates the block
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
