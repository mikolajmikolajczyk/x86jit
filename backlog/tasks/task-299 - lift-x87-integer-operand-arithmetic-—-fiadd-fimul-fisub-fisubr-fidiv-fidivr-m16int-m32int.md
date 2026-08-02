---
id: TASK-299
title: >-
  lift: x87 integer-operand arithmetic — fiadd/fimul/fisub/fisubr/fidiv/fidivr
  (m16int + m32int)
status: Done
assignee: []
created_date: '2026-08-02 19:15'
updated_date: '2026-08-02 19:55'
labels:
  - lift
  - x87
dependencies: []
ordinal: 329000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The x87 integer-operand arithmetic family is entirely unlifted. FpuKind carries a complete set of float-memory forms (FaddMemF64/F32, FsubMemF64/F32, FsubrMem*, FmulMem*, FdivMem*, FdivrMem*) but nothing that takes an INTEGER memory operand, so `fimul` and its five siblings all fall out as UnknownInstruction. Of the x87 integer surface only the load/store half exists today: FildI16/I32/I64 and FistpI16/I32/I64 plus FisttpI16/I32/I64.

Driven by `fimul` being needed; scoped to the whole arithmetic six because the plumbing is shared. Both halves already exist in the codebase and just need composing: the integer-memory read and its widening to F80 is exactly what the FildI16/FildI32 arms in x87.rs do, and the arithmetic itself is what the FmulMemF64/FaddMemF64/... arms do. Adding these one mnemonic at a time would pay the same plumbing cost six times over.

Twelve encodings in scope, two operand sizes each (DA /n = m32int, DE /n = m16int):
  fiadd, fimul, fisub, fisubr, fidiv, fidivr

Deliberately OUT of scope: `ficom`/`ficomp`. Those are not symmetric with the six above — they report their result in the status-word condition codes C0/C2/C3, which this codebase does not model at all (confirmed while implementing fnstenv in task-297, where the status word carries only TOP). The existing Fcomi/Fucomi work precisely because they write EFLAGS instead. Modeling C0-C3 is a separate, larger piece of work that also touches fnstenv, fcom/fcomp and fucom/fucomp, and should be its own task.

Watch the operand order on the reversed forms: fisub computes ST(0) = ST(0) - mem while fisubr computes ST(0) = mem - ST(0), and likewise for fidiv/fidivr. Note also that x87 division by zero raises the ZE status flag rather than a #DE fault, and this codebase does not model the FP exception flags — so fidiv by zero should produce the correct infinity per the SDM rather than trapping.

No guest trap output backs this one: the encodings come from the Intel SDM and should be pinned with an llvm-mc or objdump witness, the way task-296 pinned vextractf128.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 All six mnemonics lift for both m16int and m32int operand sizes
- [x] #2 The reversed forms (fisubr, fidivr) are verified to use the correct operand order, not silently mirrored from the non-reversed ones
- [x] #3 Encodings pinned by an llvm-mc or objdump witness
- [x] #4 jit==interp coverage, plus differential validation against Unicorn the way the other x87 work is validated
- [x] #5 ficom/ficomp remain unlifted and the reason (unmodeled C0-C3) is stated in the code rather than left implicit
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented 2026-08-02. Twelve encodings, witnessed with llvm-mc and re-read with objdump: da 00/08/20/28/30/38 (m32int) and de with the same /n (m16int). Composed from pieces that already existed - the int->F80 widening from the Fild arms and the arithmetic from the F*MemF64 arms - so ZERO new JIT plumbing (emit_x87 passes kind as u16 to the existing helpers.x87, no Helpers field, no aarch64 stub hazard). Size dispatch refuses any width other than 2/4 rather than falling through to 32-bit, since this family has no 64-bit form. Reversed forms PROVEN not swapped by mutation testing, not merely asserted: swapping fisub<->fisubr fails both tests with 7 diverging bytes (sign-byte flips 0x40<->0xc0), swapping fidiv<->fidivr fails with ~22 diverging bytes including the +-inf slots, and lifting m16 as m32 fails on the 0x0001_0007 width slot. Test values are deliberately asymmetric so a swap cannot pass: (ST0=100.0, mem=7) -> 93 vs -93, (12.0, 96) -> 0.125 vs 8.0, (-3.5, -9) -> 5.5 vs -5.5. Division by zero needed no new code - F80::div already returns inf(sign) for a zero divisor, and hardware confirms +-100/0 -> +-inf with no fault. Results stored with fstp tbyte so the Unicorn comparison is on the full 80-bit significand, not an f64 truncation. ficom/ficomp left unlifted with the reason (unmodeled status-word C0/C2/C3) stated in code at x87.rs and cross-referenced from lift/control.rs. Compat artifact unchanged as predicted - the probe never encodes these single-memory-operand shapes, so no ratchet entries were needed. FOUND ALONGSIDE, NOT FIXED: F80::div is off by 1 ULP on inexact quotients (task-300) - pre-existing, f80.rs is untouched here, and the already-lifted float fdiv forms reproduce the same wrong bits. Verified independently on main by calling F80::div directly and comparing against long double on this host: 3 of 4 probe cases diverge, in BOTH directions. Two divergent cases are excluded from the Unicorn assert with the measured hardware bytes written into the doc comment, and covered instead by x87_int_arith_equals_float_arith asserting the integer form is bit-identical to the float form. Gates on the merged main tree: 919 passed / 8 skipped, clippy clean, fmt clean, aarch64 check clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
