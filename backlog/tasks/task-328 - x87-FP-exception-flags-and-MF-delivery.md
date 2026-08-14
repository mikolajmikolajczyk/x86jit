---
id: TASK-328
title: 'x87 FP exception flags and #MF delivery'
status: In Progress
assignee: []
created_date: '2026-08-12 08:49'
updated_date: '2026-08-14 16:05'
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
AC#2 HALF DONE — stack OVERFLOW only, and the split is deliberate rather than a stopping point of convenience.

Overflow lands entirely inside `push_raw`: every push funnels through those three lines (`fld`, `fild`, `fld1`, `fldz`, and the transcendentals that push a second result), so the check needed zero call-site churn. It sets IE and SF, C1 = 1, writes the QNaN indefinite when masked and abandons the instruction with TOP unmoved when not. `Exc::SF` is carried at bit 6 and recorded outside the 0x3f masking window, since SF is a qualifier on IE and not itself maskable. Host-witnessed by `a_ninth_push_is_a_stack_overflow`, WITH C1 in the compared bits — overflow and underflow set the same two flags and C1 is the only thing that separates them, so a test ignoring it would not be testing the distinction. Two negative controls.

UNDERFLOW IS NOT DONE, and here is the obstacle so nobody re-derives it. The SDM's definition is 'an instruction references an EMPTY register as a source operand, including attempting to write the contents of an empty register to memory' (§8.5.1.1) — so the detection point is the READ, not the pop. `st()` is that read, it takes `&CpuState`, and it has ~30 call sites in `x87.rs`; raising from it needs a `&mut` migration, and several arms are shaped `set_st(cpu, i, st(cpu, j))`, which has to split into two statements to satisfy the borrow checker. Mechanical and compiler-verified, but not a change to make carelessly.

Doing it in `pop()` instead would be the tempting shortcut and would be WRONG in a way that looks right: it catches `fdivp` on an empty stack but misses `fadd st0, st3` where ST(3) is empty, which is a read without a pop. Half a rule reported as a whole one.

REMAINING after this: underflow (with C1 = 0), AC#4 (ficom/ficomp, which needs C0/C2/C3), and DE.

VERIFIED: 838/838 debug and release, clippy --all-features, fmt, and the full 169-rung ladder.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
