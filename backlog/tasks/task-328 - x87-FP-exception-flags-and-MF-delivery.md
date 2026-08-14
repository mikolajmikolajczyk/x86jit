---
id: TASK-328
title: 'x87 FP exception flags and #MF delivery'
status: In Progress
assignee: []
created_date: '2026-08-12 08:49'
updated_date: '2026-08-14 20:38'
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
- [x] #2 SF and C1 are set on stack overflow and underflow, using the emptiness state task-324 added
- [x] #3 An unmasked exception is reported on the following x87 instruction as a distinct Exit, not on the instruction that caused it
- [x] #4 ficom/ficomp lift, since C0/C2/C3 are then modelled
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#2 COMPLETED (underflow), AC#4 DONE, DE DONE. All four criteria closed 2026-08-14.

UNDERFLOW. `st()` became `st_operand(&mut CpuState) -> Option<F80>`, plus an `operand!` macro so each of the 39 readers says what it does about a fault. `None` = abandon, which is the type making the SDM's 'the FPU stops further execution' non-optional at the call site rather than a comment. Only TWO borrow conflicts fell out (`set_st(cpu, 0, operand!(cpu, 0).abs())`); the tuple reads compiled unchanged because the macro expands to a `match` whose borrow ends before the next begins.
Both shapes are host-witnessed, and the second is the reason the migration was worth doing: `fstp` on an empty stack POPS, so a check in `pop()` would catch it — `fld st(3)` with ST(3) empty reads and PUSHES, so a `pop()`-based check cannot see it at all.

AC#4 — ficom/ficomp lift, all four relations plus NaN against hardware (SDM Vol 2A Table 3-28). Worth recording: `F80::compare` ALREADY returned exactly the C3/C2/C0 triple. It is written `(zf, pf, cf)` for `fcomi` because the architectural mapping IS ZF<-C3, PF<-C2, CF<-C0 (Vol 1 §8.1.4, Figure 8-5) — the same three bits under two names, so there is one comparison rule and not two. Allowlisted in the coverage ratchet with the reason: ficom reports ONLY through the condition codes, which the differential snapshot does not carry, so a fuzzer entry would compare two engines on a value neither exposes and report agreement regardless.

DE — and my reading of the manual was WRONG, corrected by measurement. §4.9.1.2 says 'if an ARITHMETIC instruction attempts to operate on a denormal operand', which reads as excluding `fld`. Measured: `fld qword` of a denormal with nothing else in the program leaves the host at 0x3802. The reason is that `fld m32/m64` CONVERTS to double extended — the conversion is what meets the denormal, and after it the register holds an ordinary 80-bit value, so nothing downstream could ever notice. A denormal only survives in a register via `fld tbyte`, which is a pure move. All three paths (narrowing load, memory arithmetic operand, 80-bit register operand) are host-arbitrated rather than asserted from the manual.

Six negative controls across the three pieces, each failing as it must.

VERIFIED: 843/843 debug and release, clippy --all-features, fmt, aarch64 cross-check, and the full 169-rung ladder.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
