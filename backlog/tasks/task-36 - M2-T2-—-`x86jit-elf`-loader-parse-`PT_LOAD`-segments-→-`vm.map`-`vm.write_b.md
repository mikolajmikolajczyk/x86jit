---
id: TASK-36
title: 'M2-T2 — `x86jit-elf` loader: parse `PT_LOAD` segments → `vm.map` + `vm.write_b'
status: Done
assignee: []
created_date: '2026-07-06 11:05'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:elf'
milestone: m2-first-program
dependencies: []
ordinal: 36000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`x86jit-elf` loader: parse `PT_LOAD` segments → `vm.map` + `vm.write_bytes` each; return `e_entry`. Static, x86-64 only. Optional but recommended for the test. (§4.2, §12 M2)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m2-first-program).
<!-- SECTION:FINAL_SUMMARY:END -->
