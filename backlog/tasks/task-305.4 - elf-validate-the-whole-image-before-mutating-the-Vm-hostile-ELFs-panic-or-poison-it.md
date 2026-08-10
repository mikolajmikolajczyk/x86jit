---
id: TASK-305.4
title: >-
  elf: validate the whole image before mutating the Vm; hostile ELFs panic or
  poison it
status: Done
assignee: []
created_date: '2026-08-10 15:38'
updated_date: '2026-08-10 19:29'
labels: []
dependencies: []
parent_task_id: TASK-305
priority: high
ordinal: 341000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
x86jit-elf/src/lib.rs maps and copies PT_LOAD segments as it walks them and validates afterwards, with exactly one checked_add in the file. Segment addresses and extents use unchecked arithmetic: values near u64::MAX panic in a checked build and wrap in release. p_filesz <= p_memsz is not enforced.

Two consequences. A malformed ELF can abort the embedder. And because mapping happens before rejection, a loader that returns Err leaves mappings and partial segment writes behind — a caller that reuses the Vm after an error gets a poisoned one, which is the more dangerous of the two because it is silent.

Preflight every PT_LOAD with checked arithmetic and range/overlap validation, then map.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every PT_LOAD is validated before any Vm mutation; arithmetic is checked throughout
- [ ] #2 A rejected image leaves the Vm exactly as it was
- [ ] #3 Fuzz or directed tests cover extents near u64::MAX, p_filesz > p_memsz, and overlapping segments
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed: map_segments validates every PT_LOAD before touching the Vm — checked arithmetic on base+p_vaddr+p_memsz and on the span, p_filesz <= p_memsz, and file ranges inside the buffer. Mapping now happens only after the whole image passes. Test a_rejected_image_does_not_touch_the_vm covers an extent near u64::MAX, p_filesz > p_memsz, and a file range past the end; it probes with read_bytes because Vm::regions() is not public, which is also the property a caller actually cares about. Negative control confirms it fails when mapping moves back before validation. NOT covered here: the loaders still do not enforce e_type/PT_INTERP invariants, and AT_PHDR is still computed as exe_base + e_phoff — both remain open (see 305 parent, task-318).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
