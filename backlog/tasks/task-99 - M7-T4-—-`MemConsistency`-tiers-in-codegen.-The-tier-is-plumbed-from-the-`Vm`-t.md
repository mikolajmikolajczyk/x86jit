---
id: TASK-99
title: M7-T4 — `MemConsistency` tiers in codegen. The tier is plumbed from the `Vm` t
status: Done
assignee: []
created_date: '2026-07-06 11:07'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
  - 'crate:cranelift'
milestone: open-backlog
dependencies: []
ordinal: 99000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`MemConsistency` tiers in codegen. The tier is plumbed from the `Vm` through `Backend::materialize` into codegen; ordinary guest loads/stores route through `gload`/`gstore`, which emit fences on an aarch64 host (`Fast`=bare LDR/STR, `AcqRel`=fence-after-load + fence-before-store, `FullTso`=fence-after-store too). x86 stays plain (native TSO) so every tier is byte-identical there. Proven on the ARM CI runner by a deterministic codegen test asserting the `DMB ISH` count per tier (`tiers_emit_the_right_aarch64_barriers`) and a lock-free message-passing litmus (`tests/tso.rs`). **Follow-up:** use `LDAPR`/`STLR` (RCpc) for a leaner `AcqRel` than the full-`DMB` mapping; provoking an actual `Fast` reorder on the virtualized ARM runner didn't manifest (reorder rate ≈ nil there). (§8.2.3, §11)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
