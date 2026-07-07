---
id: TASK-66
title: 'M5-T4 — Correctness axis: run the whole corpus through the config matrix; the'
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:tests'
milestone: m5-performance
dependencies: []
ordinal: 66000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Correctness axis: run the whole corpus through the config matrix; the optimization ON must equal the interpreter base. Test each opt **separately** (`JitOpt(Opt::X)`), not only all-on, so a breakage is localizable. (T§8.1)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m5-performance).
<!-- SECTION:FINAL_SUMMARY:END -->
