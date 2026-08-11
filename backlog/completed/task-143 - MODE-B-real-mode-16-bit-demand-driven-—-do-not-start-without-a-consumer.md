---
id: TASK-143
title: 'MODE-B: real mode 16-bit (demand-driven — do not start without a consumer)'
status: Done
assignee: []
created_date: '2026-07-10 10:33'
updated_date: '2026-08-11 11:01'
labels:
  - guest-modes
dependencies:
  - TASK-141
priority: low
ordinal: 228000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real-address mode: segment*16 + offset in `effective_address` (seam §17.5), segment registers as plain bases, 16-bit default operand/address size (66h/67h flip the other way), IVT-based `int n` delivery, 64 KiB wraps. Reuses all MODE-A plumbing (TASK-141.1 mode threading, per-mode block key).

DEMAND-DRIVEN: start only when a machine-embedder consumer exists (DOSBox-class project) or by explicit maintainer decision — spec §17.6 forbids building unvalidated mode machinery. Full protected mode (C1 descriptors/limits/exceptions, C2 paging/softmmu, V86) stays out of the backlog entirely until then; this task is the marker for where that conversation resumes (see TASK-141 description).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Freestanding 16-bit blobs (.COM-style, org 0x100) run 3-way vs unicorn UC_MODE_16
- [ ] #2 Segment arithmetic (seg*16+off, 64 KiB offset wrap) lives in effective_address only; wrap + cross-segment cases unicorn-diffed
- [ ] #3 int n / iret deliver through the IVT with correct 16-bit frames (unicorn-diffed frame contents)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Closed into backlog/docs/deferred.md 2026-08-11 — the content is a decision not to build this, and deferred.md is where that belongs. Carrying it as an open task made the board claim work that nobody intends to start, and duplicated the document whose whole job is to say 'do not add this unprompted'.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
