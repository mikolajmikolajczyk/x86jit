---
id: TASK-76
title: >-
  M7-T4b (atomics) — Explicit sync lowers to real atomics in every tier: `lock
  add`/`sub`/`
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
  - 'crate:cranelift'
milestone: m7-multithreading-tso
dependencies: []
ordinal: 76000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Explicit sync lowers to real atomics in every tier: `lock add`/`sub`/`and`/`or`/`xor`/`inc`/`dec`, `xadd`, `xchg` (mem) → `AtomicRmw`; `cmpxchg` (mem) → `AtomicCas`. Both backends use genuine atomics (host `AtomicU*` in the interpreter, Cranelift `atomic_rmw`/`atomic_cas` in the JIT); flags come from a separate ALU op on the atomically-read old value, so locked ops flag exactly like their plain forms. `tests/threads.rs::contended_counter_*` proves atomicity (deterministic `THREADS*INCS` on both backends); `atomics_match_unicorn` matches the real CPU. **Remaining:** `mfence` → `DMB ISH` (a no-op on x86; wire with the barrier tiers). Misaligned locked ops fall back to a non-atomic RMW (same value, rare). (§8.2.3)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m7-multithreading-tso).
<!-- SECTION:FINAL_SUMMARY:END -->
