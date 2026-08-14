---
id: TASK-328
title: 'x87 FP exception flags and #MF delivery'
status: In Progress
assignee: []
created_date: '2026-08-12 08:49'
updated_date: '2026-08-14 15:58'
labels:
  - m8-simd
dependencies: []
ordinal: 364000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The x87 tracks the status word's exception flags as storage and raises nothing. `fldenv`/`fnstenv` round-trip them (task-324) so a `fenv_t` save/restore is exact, but no operation SETS a flag and no unmasked exception is ever delivered.

What is missing, in order of what a guest would notice:

**Flags are never set.** IE/DE/ZE/OE/UE/PE stay at whatever was loaded. An invalid operation (the QNaN indefinite paths in `f80.rs`), a divide by zero, an overflow to infinity, an underflow to a denormal and every inexact result should each set their bit and the ES/B summary. `f80.rs` already knows which case it is at every site — the arms that return `F80::indefinite()` are exactly the invalid ones — so this is about returning that alongside the value rather than discovering it.

**Unmasked exceptions are never delivered.** x86 reports an unmasked x87 exception on the NEXT floating-point instruction, not on the one that caused it (SDM Vol 1 §8.7), as #MF. That needs a pending-exception bit, a check at the head of every x87 op, and an `Exit` the embedder can turn into SIGFPE.

**The stack fault flag (SF) and C1.** A stack overflow (push onto a non-empty ST(7)) or underflow (pop an empty register) sets IE and SF, with C1 saying which. Stack emptiness is real state since task-324, so the condition is now detectable; nothing checks it.

**C0/C2/C3.** Not set by anything, which is why `ficom`/`ficomp` and `fprem`'s partial-remainder loop stay unlifted or approximate.

Split out of task-324, whose AC#2 asked for restored flags to affect later execution — the state half landed there, this is the behaviour half. Note the SSE counterpart stays deferred (`deferred.md`, MXCSR): a guest that reads MXCSR's flags is a different consumer and there is no evidence of one.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 An invalid operation, divide-by-zero, overflow, underflow and inexact each set their status-word flag, witnessed against hardware
- [ ] #2 SF and C1 are set on stack overflow and underflow, using the emptiness state task-324 added
- [x] #3 An unmasked exception is reported on the following x87 instruction as a distinct Exit, not on the instruction that caused it
- [ ] #4 ficom/ficomp lift, since C0/C2/C3 are then modelled
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#3 DONE (2026-08-13/14). Two halves, both from SDM Vol 1 §8.6, and the second is easy to miss:

- **Deferred report.** The FPU signals on the faulting instruction, but the processor 'checks the ES flag ... on the NEXT occurrence of a floating-point instruction or a WAIT/FWAIT' and traps there. Checked at the head of every waiting op; reporting on the causing instruction would leave the guest's RIP an instruction early, which is the entire reason the rule exists. No new `Exit`: `Exception { vector: 16 }` is #MF, and the JIT already had `RET_EXCEPTION` + `MemCtx.exception_vector`.
- **The instruction is ABANDONED.** 'the x87 FPU stops further execution of the floating-point instruction' — so no result is written and TOP does not move. `raise()` now returns whether to abandon, and the seven commit sites honour it.

THE NO-WAIT LIST IS LOAD-BEARING. §8.6 enumerates FNINIT, FNSTENV, FNSAVE, FNSTSW, FNSTCW, FNCLEX as not checking. A handler READS the status word with FNSTSW and CLEARS it with FNCLEX — if those waited, a guest would trap, enter its handler, trap again on the handler's first instruction, and never get out. Pinned by `a_handler_can_read_and_clear_the_status_word`. Approximation recorded on `waits_for_pending`: the lift folds each waiting form onto its no-wait twin's `FpuKind`, so `fclex` does not wait either.

TWO THINGS THE NEGATIVE CONTROLS CAUGHT, both the same shape as the rest of this session:

1. Deleting the abandon-on-unmasked logic broke NOTHING — eleven tests watched flags, and flags are set either way. Added `an_unmasked_exception_leaves_the_stack_untouched`, which observes TOP through `fnstenv` (non-waiting, so it runs with ES pending — that is what makes the property observable at all).
2. The JIT variant of the #MF test was RUNNING ON THE INTERPRETER: a scripted edit left the `backend` parameter unused, and clippy's unused-variable error is what exposed it. Once it really ran on the JIT it failed — `emit_x87` only tested for `RET_UNMAPPED`, so the helper's `RET_EXCEPTION` was ignored and the block ran on to `hlt`. A helper's return code does nothing unless generated code tests for it. Fixed with `trap_if_unmapped_or_exception`.

REMAINING: AC#2 (SF and C1) and AC#4 (ficom/ficomp, which needs C0/C2/C3), plus DE. AC#2's obstacle is mechanical: `st()` takes `&CpuState` and is called from ~30 sites, so raising underflow from it needs a `&mut` migration.

VERIFIED: 837/837 debug and release, clippy --all-features, fmt, aarch64 cross-check, and the full 169-rung ladder.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
