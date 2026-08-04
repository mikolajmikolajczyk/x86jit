---
id: doc-35
title: LLVM x86-64 i128 miscompile — the defect behind the vmovss wrong-result bug
type: other
created_date: '2026-07-29 14:27'
updated_date: '2026-07-29 14:27'
---

An optimized build of `x86jit-core` computed the wrong answer for one instruction.
The cause is not in this repo: LLVM's x86-64 backend miscompiles a specific i128
pattern. This document is the evidence, kept so the workaround can be removed when a
fixed toolchain is the floor — and so the next person does not re-derive it.

Reproducer and runner live beside this file. `./run.sh` prints the whole picture and
exits 1 **if the toolchain has been fixed**, which is the signal to revisit
`exec_v_float_mov`.

## The defect

Two conditions, both required:

1. an i128 mask produced by `select i1` between two **constants**, and
2. the other operand loaded in a **predecessor** basic block.

Remove either and the generated code is correct. The variant files are exactly that
matrix — `v_sel_pred.ll` (both, broken), `v_sel_same.ll`, `v_const_pred.ll`,
`v_const_same.ll` (one condition each, correct). `run.sh` also retargets all four to
aarch64 and runs them under qemu, with a deliberately-broken fifth module as the control
that the leg works.

The high 64 bits of the result should be the high 64 bits of the masked operand,
because both select constants (`-2^64` and `-2^32`) have an all-ones high word. The
backend emits a literal zero there:

    movq   %rdx, (%rax,%rcx)        ; low half — correct bit-select
    movq   $0,   8(%rax,%rcx)       ; high half — should be the operand's high word

## Why it is LLVM's bug and not ours

The decisive measurement is LLVM disagreeing with itself on one module:

    want         hi=aaaaaaaaaaaaaaaa lo=bbbbbbbb3fb6cd8e
    lli --force-interpreter   hi=aaaaaaaaaaaaaaaa lo=bbbbbbbb3fb6cd8e
    lli (JIT, x86-64 backend) hi=0000000000000000 lo=bbbbbbbb3fb6cd8e

The interpreter evaluates IR semantics directly; the JIT uses the same backend as
`llc`. No defect in the reproducer's IR explains one being right and the other wrong.

Supporting, in the order it was established:

* wrong at **every** `llc` opt level, `-O0` through `-O3` — so this is i128
  legalization / ISel, not an optimization pass;
* `-opt-bisect-limit` never flips it, though it gates IR passes — consistent;
* the optimized LLVM IR is **correct**: full `load i128` / `and` / `or disjoint` /
  `store i128`;
* Miri reports no UB of ours on the executed path, under both Stacked Borrows and
  Tree Borrows;
* affected: LLVM 22.1.5, 22.1.7, 22.1.8, and rustc 1.96.1's own LLVM 22.1.2, target
  `x86_64-unknown-linux-gnu`. Not checked: older majors, LLVM trunk.

## aarch64 is NOT affected

The same IR, retargeted and run under `qemu-aarch64`, is correct on all four variants.
The backend loads and stores both halves (`ldp` / `stp`), which is what x86-64 fails to
do. Measured, not read off the disassembly — and with a positive control: a fifth module
that zeroes the high half in the IR itself does report `MISCOMPILED` through the same
harness, so the aarch64 leg can detect a wrong answer and is not just passing by default.

This matters to us beyond the upstream question. **ARM is the primary target, so shipped
ARM builds were never wrong.** What was wrong is x86-64 — which is the host for the
differential/oracle work, and the host the embedder that reported this actually runs on.
The wrong-answer window was therefore x86-host only, and the ARM path stayed honest
throughout.

## Why x86jit hit it

`FPrec::bytes()` is a two-armed match, so `lane_mask(prec.bytes())` becomes a select
between two i128 constants — condition 1. The source operand is computed before the
destination's bounds-check branch, so it lands in a predecessor block — condition 2.
Both, by construction, in `exec_v_float_mov`, and in nothing else.

The workaround (task-223) merges byte-wise and has no i128 select mask at all.
`exec_v_insert_lane` is the other site that reads a different vector register than it
writes, but its mask is `lane_mask(*size) << sh` — a shift, not a select of two
constants — so it does not meet condition 1. **Treat any future i128 mask derived from
a two-valued enum as a candidate.**

## Not reported upstream

Deliberate, 2026-07-29: filing well means understanding the backend well enough to say
which component is at fault, and that has not been done. The bundle here is complete
enough to file whenever someone wants to — `llvm/llvm-project` issues, labels
`backend:X86`, `miscompilation`, `llvm:SelectionDAG`. Check LLVM trunk first; all
versions measured are on the 22.1.x branch, and a fix upstream would make the report
moot. A duplicate search turned up nothing matching, but it was shallow.

## How the reproducer was obtained, and one dead end

Reduced from the real crate: the bug first reproduces through `interpret_block` with a
hand-built `IrBlock`, which removes the decoder and the lift entirely.

`llvm-reduce` was run twice and **both results were false positives** that an
asm-pattern interestingness test could not tell from the real thing. The first
exploited a 14-line scan window — the high-half store sat one line past it. The second
collapsed the load and the store onto the same pointer, where narrowing to 64 bits is
legal. Both were caught by controls, not by eye.

The lesson generalises: an oracle that greps generated assembly cannot separate a wrong
narrowing from a legal one. The oracle has to execute. The reproducer that stands was
built by hand from the real IR's block structure and confirmed by the variant matrix.

## Related

* task-223 — the wrong-result bug and its byte-wise workaround
* task-227 — the suite only ran in debug, which is why this survived
* task-228 — a real, unrelated guest-RAM provenance bug found while chasing this
* task-229 — this investigation
