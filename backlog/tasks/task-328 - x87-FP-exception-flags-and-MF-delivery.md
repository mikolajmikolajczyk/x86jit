---
id: TASK-328
title: 'x87 FP exception flags and #MF delivery'
status: To Do
assignee: []
created_date: '2026-08-12 08:49'
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
- [ ] #1 An invalid operation, divide-by-zero, overflow, underflow and inexact each set their status-word flag, witnessed against hardware
- [ ] #2 SF and C1 are set on stack overflow and underflow, using the emptiness state task-324 added
- [ ] #3 An unmasked exception is reported on the following x87 instruction as a distinct Exit, not on the instruction that caused it
- [ ] #4 ficom/ficomp lift, since C0/C2/C3 are then modelled
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
