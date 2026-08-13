---
id: TASK-328
title: 'x87 FP exception flags and #MF delivery'
status: In Progress
assignee: []
created_date: '2026-08-12 08:49'
updated_date: '2026-08-13 21:53'
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
- [ ] #3 An unmasked exception is reported on the following x87 instruction as a distinct Exit, not on the instruction that caused it
- [ ] #4 ficom/ficomp lift, since C0/C2/C3 are then modelled
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#1 landed 2026-08-13; AC#2/#3/#4 remain. Every case is witnessed against the REAL CPU, and the harness asserts host-vs-expectation BEFORE engine-vs-host, so a wrong expectation blames the test rather than sending someone hunting in the engine. That ordering paid for itself twice in one sitting.

WHAT LANDED. `f80::Exc` — the six flags in their status-word bit positions — returned alongside the value by every `*_ctl` op, because the two conditions that matter cannot be recovered from the result: an inexact result looks exactly like an exact one, and a masked overflow's largest-finite is an ordinary number. `x87::raise` ORs them into `fpu_sw` (sticky, SDM Vol 1 §8.1.3.3) and derives ES/B from the CURRENT masks, not per-op — 'if an exception flag is masked, the x87 FPU will still set the appropriate flag ... but it will not set the ES flag', and B 'reflects the contents of the ES flag'.

TWO DEFECTS THE HARDWARE WITNESS FOUND, neither of which was on the task's list:

1. MASKED OVERFLOW IGNORED THE ROUNDING MODE. `finish` returned infinity for every mode; SDM Vol 1 Table 4-11 returns the largest FINITE value for three of the four. Fixing it changed nothing in 831 tests — nothing covered it — so `masked_overflow_follows_the_rounding_mode` now compares all eight (mode x sign) results against the host byte-for-byte and asserts they are not all collapsed to two values.

2. DENORMALIZATION LOSS WAS NOT COUNTED AS INEXACT. `min_normal * min_normal` is 2^-32764: exact as a product, nothing rounded away, and still unrepresentable, so encoding flushes the whole significand. Counting only the ROUNDING loss reported no exception where the host reports UE and PE. Underflow's inexactness includes denormalization loss (SDM Vol 1 §4.9.1.5).

The masked/unmasked underflow rule is spelled out rather than folded into one condition, because they differ: masked reports 'only when the result is both tiny and inexact', unmasked 'when the result is non-zero tiny, regardless of inexactness'. From memory I would have written the first for both.

ALSO: the compiler reported the flag-less rounding wrappers DEAD immediately after I wrote a doc comment justifying them as 'for the internal multi-step users'. There are none. Deleted, and the comment now says why there is deliberately no convenience overload — one nobody needs is how a site quietly stops reporting.

TWO OF MY OWN TEST BUGS, both caught by the host-first assertion: 1e300*1e300 does not overflow double-extended (its range reaches ~1.19e4932, so no pair of f64 operands can), and the operand slots were 8 bytes apart, so the 10-byte tbyte forms overlapped and B's exponent word landed on A's.

NOT DONE: DE (denormal operand) is not raised — `from_bytes` folds denormals into `Class::Normal`, so the information is gone by the time arithmetic sees it, and a pseudo-denormal is indistinguishable from a normal at biased exponent 1. Detecting it wants either a flag on `F80` or detection at the load site. AC#2 (SF/C1), AC#3 (#MF on the following instruction — `Exit::Exception { vector: 16 }` already exists, so no new Exit is needed) and AC#4 (ficom) are untouched.

Four negative controls, each failing as it must: drop the ZE arm, stop deriving ES from the masks, stop counting denormalization loss, revert Table 4-11.

VERIFIED: 831/831 debug and release, clippy --all-features, fmt, and the full 169-rung ladder.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
