---
id: TASK-317
title: Vector MMIO exits can never be completed
status: Done
assignee: []
created_date: '2026-08-10 15:40'
updated_date: '2026-08-11 11:04'
labels: []
dependencies: []
priority: medium
ordinal: 353000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
interp/vector.rs (~17): vector loads and stores re-call vload/vstore unconditionally after an MMIO exit and never consume the value or acknowledgement that complete_mmio_read / complete_mmio_write installed. The retry produces the same exit, forever. A 16-byte store additionally exposes only 'v as u64' in the exit while declaring size == 16, so the embedder is handed half the transaction and told it is whole.

Vector access to a Trap region is therefore unrecoverable. It has not been hit because no current consumer puts a device behind a vector access — but MMIO is an embedder-facing contract, and this one cannot be honoured.

Either consume the completion state on every vector path, or reject widths the MMIO ABI cannot encode so the embedder gets a defined refusal instead of a livelock.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A vector load/store to a Trap region completes after the embedder answers, or is refused with a defined error
- [ ] #2 The exit payload carries the whole transaction, or the width is rejected
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Merged into task-305 2026-08-11. Both are the resumability contract for a trapped instruction seen from the embedder's side: one leaves partial state behind, the other cannot be finished at all. Same fix discipline, same tests.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
