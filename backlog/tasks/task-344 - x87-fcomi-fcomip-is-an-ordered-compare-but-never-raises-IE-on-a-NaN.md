---
id: TASK-344
title: 'x87: fcomi/fcomip is an ordered compare but never raises IE on a NaN'
status: To Do
assignee: []
created_date: '2026-08-15 13:25'
labels:
  - m6-x87
dependencies: []
priority: medium
ordinal: 380000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
engine   fcomi  sw=0x3000 IE=0      fucomi sw=0x3000 IE=0
    hardware fcomi  sw=0x3001 IE=1      fucomi sw=0x3000 IE=0

SDM Vol 1 Table 8-10, 'Ordered compare and test operations: one or both operands are NaNs' -> #IA with the condition codes set to 111. The engine's own FicomI* arm at x87.rs:1139 gets this right for ficom and cites the same rule; the fcomi arm (x87.rs:1089-1100) shares neither the check nor the comment. Hardware separates ordered from unordered exactly as the manual does — fucomi correctly stays silent for a QNaN.

Guest sees a missing IE flag, and with IM unmasked a missing trap: fcomi against a QNaN must abandon and deliver #MF, and here it silently produces ZF/PF/CF = 111.

Sites: 1 arm covering 4 kinds; only Fcomi/Fcomip are affected.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 fcomi/fcomip raise IE for any NaN operand and abandon when IM is unmasked; fucomi/fucomip keep their QNaN-silent behaviour
- [ ] #2 A host-witnessed test covers QNaN and SNaN across all four kinds
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
