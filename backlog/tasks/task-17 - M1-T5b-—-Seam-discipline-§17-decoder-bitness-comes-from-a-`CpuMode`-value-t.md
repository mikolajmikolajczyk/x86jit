---
id: TASK-17
title: >-
  M1-T5b — Seam discipline (§17): decoder bitness comes from a `CpuMode` value
  (t
status: Done
assignee: []
created_date: '2026-07-06 11:04'
labels: []
milestone: m1-ir-interpreter
dependencies: []
ordinal: 17000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Seam discipline (§17): decoder bitness comes from a `CpuMode` value (today only `Long64`), NOT the literal `64`; keep `effective_address` (M1-T2) the *single* place any address is computed. Leave the seams, build no mode machinery (no `trait ExecutionMode`, no `Protected32` API). (§17.3, §17.5, §17.6)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
