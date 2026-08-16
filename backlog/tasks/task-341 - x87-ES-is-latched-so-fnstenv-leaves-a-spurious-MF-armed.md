---
id: TASK-341
title: 'x87: ES is latched, so fnstenv leaves a spurious #MF armed'
status: To Do
assignee: []
created_date: '2026-08-15 13:25'
labels:
  - m6-x87
dependencies: []
priority: high
ordinal: 377000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`Fnstenv` does `cpu.fpu_cw |= 0x3f` (masking everything) but leaves ES set. `raise` only ever ORs ES in, never clears it, and `mf_pending_before` tests the raw ES bit.

    engine   exit = Exception { addr: 4121, vector: 16 }
    hardware runs to completion: sw_before=0xb884, cw_after_fnstenv=0x037f,
             sw_after_fld1=0x3004  (ZE still set, ES=0)

Negative control on hardware: the same program WITHOUT the fnstenv dies with SIGFPE (exit=136), so the fld1 really is the reporting point and the fnstenv really is what disarms it.

SDM Vol 1 §8.6: 'When an FNINIT, FNSTENV, FNSAVE, or FNCLEX instruction is executed, all pending exceptions are essentially lost (either the x87 FPU status register is cleared or all exceptions are masked).'

Guest sees a spurious trap, and in the classic shape a LIVELOCK: an x87 #MF handler whose first act is fnstenv — the canonical opening, and what feholdexcept/fesetenv compile to — re-traps on the first waiting FP instruction after it and never makes progress.

The doc on `raise` claims ES 'is recomputed from the whole status word against the current masks'. It is not; it is set-only. The same shape applies to `fldcw` masking an already-set flag. The fldenv doc at x87.rs:549-553 describes only the unmask direction, which is benign, not this one.

Applies to both backends: they share exec_x87 and mf_pending_before.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 ES (and B) are recomputed against the current masks wherever the masks change — fnstenv, fnsave, fninit, fldcw, fldenv — rather than being set-only
- [ ] #2 A test runs the handler shape: raise a masked-off exception, fnstenv, then a waiting FP instruction, and requires no trap
- [ ] #3 The raise doc says what the code does
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
