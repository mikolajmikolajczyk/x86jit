---
id: TASK-148
title: GP-1 — guard-page protect-callback plumbing (dark)
status: Done
assignee: []
created_date: '2026-07-07 11:02'
updated_date: '2026-07-07 11:10'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
GP-1 landed. HostRam gains protect: Option<ProtectFn> (embedder guard-page hook, default None → pre-GP-1 behavior); Memory::map/unmap call reprotect() to open/close the region's host pages (round outward on map; on unmap close page-by-page skipping any page a surviving region overlaps → shared-edge safe); HOST_PAGE=4096. hostmem::reserve_guarded (PROT_NONE + mprotect callback) beside untouched reserve. Core stays {iced-x86} (callback injected). Tests: guard_pages_map_opens_and_unmap_closes_region_pages, guard_pages_shared_edge_page_stays_open_until_last_region_unmaps (recording callback), reserve_guarded_maps_opened_regions_and_stays_sparse (RSS). 75/75 core+linux+whole_program green; clippy+fmt clean. Dark: nothing user-visible yet (reserve() still RW; GP-2 flips the caller).
<!-- SECTION:NOTES:END -->
