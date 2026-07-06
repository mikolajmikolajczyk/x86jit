---
id: TASK-16
title: >-
  M1-T5c — Decode from `Memory::code_slice(addr, ..)` (iced needs a byte slice,
  n
status: Done
assignee: []
created_date: '2026-07-06 11:04'
labels: []
milestone: m1-ir-interpreter
dependencies: []
ordinal: 16000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Decode from `Memory::code_slice(addr, ..)` (iced needs a byte slice, not scalar `read`); emit `IrOp::InsnStart { guest_addr }` at each instruction boundary — required so a mem-trap can set RIP to the faulting instruction (`guest_len` is only the block end). (§6.2, §7.3, §8, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
