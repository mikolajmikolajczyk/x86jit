---
id: TASK-316
title: F80 treats unnormal encodings as Normal and destroys NaN identity
status: To Do
assignee: []
created_date: '2026-08-10 15:39'
labels: []
dependencies: []
priority: medium
ordinal: 352000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
f80.rs (~88): from_bytes classifies any nonzero, non-max exponent as Normal without checking the explicit integer bit, so an unnormal encoding enters ordinary finite arithmetic even though the x87 tag logic elsewhere calls it special. Separately, every NaN arithmetic arm returns F80::nan(), discarding the operand's sign, payload and signaling/quiet status.

The project's claim for F80 is bit-identical 80-bit results on any host. NaN payload propagation and the unsupported-operand class are part of that claim, and neither holds. It has not surfaced because no fixture computes with a hand-built NaN payload.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Unsupported, signaling-NaN and quiet-NaN are distinct classes with x87 invalid-operation and NaN-selection rules
- [ ] #2 Raw 10-byte witnesses cover pseudo-denormals, unnormals, both NaN kinds, signs and payloads
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
