---
id: TASK-24
title: >-
  M1-T10 — Trap-out + RIP convention: after `syscall` RIP = past the
  instruction;
status: Done
assignee: []
created_date: '2026-07-06 11:04'
labels: []
milestone: m1-ir-interpreter
dependencies: []
ordinal: 24000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Trap-out + RIP convention: after `syscall` RIP = past the instruction; on memory trap RIP = the faulting instruction. Same rule the JIT will follow. (§8)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
