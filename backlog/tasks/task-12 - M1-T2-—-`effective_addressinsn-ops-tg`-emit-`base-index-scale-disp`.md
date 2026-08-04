---
id: TASK-12
title: 'M1-T2 — `effective_address(insn, ops, tg)`: emit `base + index*scale + disp`;'
status: Done
assignee: []
created_date: '2026-07-06 11:04'
updated_date: '2026-07-07 10:02'
labels:
  - 'crate:core'
milestone: m1-ir-interpreter
dependencies: []
ordinal: 12000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`effective_address(insn, ops, tg)`: emit `base + index*scale + disp`; use iced's RIP-relative value (next-insn base); add FS/GS base when a segment prefix is present. (§7.1, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
