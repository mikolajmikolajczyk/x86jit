---
id: TASK-94
title: >-
  DYN-T1 — Load a `PT_INTERP` `ET_DYN` PIE: `load_dynamic_elf` maps the exe +
  int
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:elf'
milestone: open-backlog
dependencies: []
ordinal: 94000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Load a `PT_INTERP` `ET_DYN` PIE: `load_dynamic_elf` maps the exe + interpreter at load biases; `setup_stack_dyn` builds the full auxv (`AT_PHDR/PHENT/PHNUM/BASE/ENTRY/PAGESZ/RANDOM/HWCAP/uid-gid`). Enters at the interpreter. (§4, testing.md §12)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
