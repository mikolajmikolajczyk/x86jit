---
id: TASK-25
title: >-
  M1-T10b — **Instruction atomicity** (pitfall #0, §16): within one guest
  instruct
status: Done
assignee: []
created_date: '2026-07-06 11:04'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: m1-ir-interpreter
dependencies: []
ordinal: 25000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
**Instruction atomicity** (pitfall #0, §16): within one guest instruction, emit all trapping ops (load/store) **before** all committing ops (WriteReg, flags), or prove idempotence — else a fault-retry corrupts state (`push` moving RSP before a faulting store, RMW writing flags before a faulting store). Bake the ordering into the lowering helpers. (§7 pitfall 3)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
