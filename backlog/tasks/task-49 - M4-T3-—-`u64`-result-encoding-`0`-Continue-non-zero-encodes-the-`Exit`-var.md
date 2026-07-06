---
id: TASK-49
title: 'M4-T3 — `u64` result encoding: `0` = Continue, non-zero encodes the `Exit` var'
status: Done
assignee: []
created_date: '2026-07-06 11:05'
labels: []
milestone: m4-jit-cranelift
dependencies: []
ordinal: 49000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`u64` result encoding: `0` = Continue, non-zero encodes the `Exit` variant (discriminator + data, or details written into CpuState/MemCtx). One place, shared by codegen and `run_compiled`. (§8.2.2)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m4-jit-cranelift).
<!-- SECTION:FINAL_SUMMARY:END -->
