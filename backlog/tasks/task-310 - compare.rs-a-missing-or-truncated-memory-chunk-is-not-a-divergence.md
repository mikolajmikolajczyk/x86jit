---
id: TASK-310
title: 'compare.rs: a missing or truncated memory chunk is not a divergence'
status: Done
assignee: []
created_date: '2026-08-10 15:39'
updated_date: '2026-08-10 19:29'
labels: []
dependencies: []
priority: high
ordinal: 346000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
compare.rs:182 looks up the expected chunk's address in the actual result with .find() and skips it when absent, then zips the byte slices, which stops at the shorter one.

So an oracle that returns no memory at all, or a short chunk, produces zero memory diffs and the comparison passes. Every test that proves a guest WRITE went to the right place rests on this path, which means a whole class of differential result is currently unfalsifiable.

This is the highest-value item among the testing findings: it does not fix a bug, it restores the ability to detect one.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Chunk address sets, kinds and lengths must match before bytes are compared; missing, extra and truncated chunks are reported as divergences
- [ ] #2 Regression tests with empty and shortened actual memory fail against the current comparator
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed: the comparator now requires matching chunk address sets and lengths. Divergence gained missing_mem, extra_mem and mem_len_diffs, all wired into is_empty and Display. Test absent_and_short_memory_are_divergences covers empty, truncated and extra chunks; it fails against the old comparator. The full suite stayed green (762), which says the existing tests were already returning matching chunk sets — the hole was latent, not active.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
