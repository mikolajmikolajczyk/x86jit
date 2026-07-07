---
id: TASK-148
title: GP-1 — guard-page protect-callback plumbing (dark)
status: To Do
assignee: []
created_date: '2026-07-07 11:02'
labels:
  - go-caddy
  - 'crate:core'
  - 'goal:harden'
dependencies: []
ordinal: 157000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
doc-30 GP-1 (guard-pages-sigsegv.md). HostRam gains embedder-injected protect callback (default None, ctors unchanged); Memory::map/unmap invoke it for the region page range (round outward on map, inward on unmap with shared-edge check vs remaining regions); hostmem::reserve_guarded (PROT_NONE + mprotect) beside untouched reserve. Core stays iced-x86. Tests: rounding units (recording callback incl shared-edge unmap); RSS sparseness on reserve_guarded.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
