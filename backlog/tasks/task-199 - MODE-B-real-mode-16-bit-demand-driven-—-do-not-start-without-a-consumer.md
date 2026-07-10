---
id: TASK-199
title: 'MODE-B: real mode 16-bit (demand-driven — do not start without a consumer)'
status: To Do
assignee: []
created_date: '2026-07-10 10:33'
labels:
  - guest-modes
dependencies:
  - TASK-197
priority: low
ordinal: 228000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real-address mode: segment*16 + offset in `effective_address` (seam §17.5), segment registers as plain bases, 16-bit default operand/address size (66h/67h flip the other way), IVT-based `int n` delivery, 64 KiB wraps. Reuses all MODE-A plumbing (TASK-197.1 mode threading, per-mode block key).

DEMAND-DRIVEN: start only when a machine-embedder consumer exists (DOSBox-class project) or by explicit maintainer decision — spec §17.6 forbids building unvalidated mode machinery. Full protected mode (C1 descriptors/limits/exceptions, C2 paging/softmmu, V86) stays out of the backlog entirely until then; this task is the marker for where that conversation resumes (see TASK-197 description).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Freestanding 16-bit blobs (.COM-style, org 0x100) run 3-way vs unicorn UC_MODE_16
- [ ] #2 Segment arithmetic (seg*16+off, 64 KiB wrap) lives in effective_address only
- [ ] #3 int n / iret deliver through the IVT with correct 16-bit frames
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
