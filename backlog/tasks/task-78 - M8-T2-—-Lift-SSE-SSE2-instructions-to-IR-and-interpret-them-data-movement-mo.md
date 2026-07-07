---
id: TASK-78
title: 'M8-T2 — Lift SSE/SSE2 instructions to IR and interpret them: data movement (mo'
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: m8-simd
dependencies: []
ordinal: 78000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Lift SSE/SSE2 instructions to IR and interpret them: data movement (movdqu/a, movaps/upd, movd/q, movss/sd), logic (pxor/pand/por/pandn + ps/pd aliases), packed integer arithmetic + shifts, shuffles/pack (pshufd, punpckl\*, packuswb, pinsrw), and scalar+packed float (add/sub/mul/div/min/max, sqrt, cvt\*, ucomis\*/comis\*). AVX (VEX/YMM) is a later chapter. (§12 M8+)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m8-simd).
<!-- SECTION:FINAL_SUMMARY:END -->
