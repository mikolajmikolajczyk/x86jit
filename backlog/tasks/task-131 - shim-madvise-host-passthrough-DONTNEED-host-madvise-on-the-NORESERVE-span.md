---
id: TASK-131
title: >-
  shim: madvise host-passthrough (DONTNEED -> host madvise on the NORESERVE
  span)
status: Done
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-12 19:39'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 140000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope; LOW priority (footprint). P3 lands madvise->0 no-op (correct: advisory, Go does not rely on advice-zeroing). A host-passthrough (guest range -> base+addr madvise on the NORESERVE span) reclaims RSS for a long-running caddy. Footprint optimization, not correctness.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 shim test: madvise(DONTNEED) on a guest span reads back zeros afterwards (host passthrough observable)
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged 8abd9e9 + fix). madvise(MADV_DONTNEED) now host-madvises the fully-covered inner page range of the NORESERVE backing (releases host RSS) + zeroes partial edges; Memory::host_ram_ptr scopes it to host-mapped RAM (Vec-backed/MMIO/Trap -> whole-range write-zero fallback). Zero postcondition preserved (all Host backings are MAP_ANONYMOUS|NORESERVE -> DONTNEED refaults zero; verified by real-mmap test). REVIEW-CAUGHT (Medium, fixed): div_ceil*HOST_PAGE overflowed u64 on a top-page addr -> debug host-abort from guest RDI (harden #1); fixed with checked_mul + regression test. First review misfired (checked wrong worktree via cwd-reset); re-reviewed on main. 655/655 at merge.
<!-- SECTION:NOTES:END -->
