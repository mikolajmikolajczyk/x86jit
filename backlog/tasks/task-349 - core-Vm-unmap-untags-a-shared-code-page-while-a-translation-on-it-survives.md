---
id: TASK-349
title: 'core: Vm::unmap untags a shared code page while a translation on it survives'
status: To Do
assignee: []
created_date: '2026-08-15 13:28'
labels:
  - code-review
dependencies: []
priority: high
ordinal: 385000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`invalidate_overlapping(lo, hi, ...)` (vm.rs:514-521) drops only units whose span overlaps [lo,hi), but its `on_clear_page` callback clears the code tag for EVERY page the range touches. A cached block in a neighbouring region on the same 4 KiB page survives the drop and loses its tag -- permanently, so no later write to that page invalidates it either.

`handle_smc` (vm.rs:564) and `map(Trap)` (vm.rs:467) both keep invalidate-range == clear-range. This is the one of the three sites that does not, despite a comment claiming it mirrors handle_smc.

Probe, both backends. Regions [0x1000,0x1400) and [0x1400,0x1800) share code page 1; a block at 0x1400 runs once (cached, page tagged); vm.unmap(0x1000, 0x400) removes only the OTHER region; the block at 0x1400 is patched mov eax,1 -> mov eax,2 and re-run:

    [interp/control] dirty-code pending after patch: true
    [interp/unmap]   after unmap: is_code_page(1) = false
    [interp/unmap]   dirty-code pending after patch: false
    [jit/unmap]      after unmap: is_code_page(1) = false
    control eax interp=2 jit=2      after-unmap eax interp=1 jit=1

Guest sees a wrong result from permanently stale code. This is not the documented one-block deferral -- the page never regains its tag.

Reachable from any embedder whose munmap removes a region sharing a page with another mapped region. Sub-page regions are a supported, tested shape (memory.rs guard_pages_shared_edge_page_stays_open_until_last_region_unmaps maps 0x400-byte regions).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 unmap clears code tags only for pages with no surviving translation, or re-tags the survivors
- [ ] #2 A test covers two sub-page regions sharing a code page, unmaps one, and requires the survivor to stay SMC-tracked on both tiers
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
