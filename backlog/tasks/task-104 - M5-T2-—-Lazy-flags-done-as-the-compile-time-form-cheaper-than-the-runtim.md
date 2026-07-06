---
id: TASK-104
title: 'M5-T2 — Lazy flags, done as the **compile-time** form (cheaper than the runtim'
status: Done
assignee: []
created_date: '2026-07-06 11:07'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 104000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Lazy flags, done as the **compile-time** form (cheaper than the runtime Variant B sketch): a backward liveness pass in `lift_block` narrows each ALU op's `set_flags` to the flags still live, and since the backends gate the flag *store* by the mask, Cranelift's DCE drops the dead flag computation (parity/AF/OF). Plus a **block-local GPR value cache** in the JIT (write-through, so no trap-flush; invalidated after cpuid/x87/string helpers). Together: SHA-256 JIT 28.5 ms → 18.4 ms (~35% faster, 12.2× over interp). Correct vs Unicorn. Runtime Variant B (defer to read) not needed. (§3.2)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
