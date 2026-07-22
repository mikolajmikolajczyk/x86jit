---
id: TASK-286
title: >-
  perf(codegen): lazy flags (Variant B) — cc_op/cc_src/cc_dst, ~35 -> ~6 host
  instructions per block
status: To Do
assignee: []
created_date: '2026-07-22 15:12'
updated_date: '2026-07-22 15:44'
labels:
  - perf
  - 'crate:cranelift'
  - 'crate:core'
dependencies:
  - TASK-285
ordinal: 316000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
DO NOT START before TASK-285 has landed AND the embedder has re-measured. TASK-285 is the cheap falsifiable probe for this whole direction; if a measured ~20% cut in hot code does not move fps, this task is dead and must be closed as such.

Recorded in backlog/docs/deferred.md as 'Lazy flags (Variant B)', deferred at design time with 'revisit when M5, once the JIT works and profiling shows flag computation is hot'. task-282 is that profiling. TASK-104 delivered the compile-time substitute (dead-flag elimination); this is the runtime form the substitute was standing in for.

THE MEASUREMENT (task-282, density_tests with a chained exit):
  - a real block on the embedder's workload averages 2.9 guest instructions and emits ~62 host instructions
  - 35 of those 62 are flag materialization: CF 4, PF 7, AF 7, ZF 5, SF 4, OF 8
  - marginal cost of an extra ALU instruction inside a block is only 3.0 host instructions, because the mid-end already kills flags overwritten within the block
  - so the 35 is paid ONCE PER BLOCK regardless of block length: the last flag-setting instruction's flags are live across the block boundary and must reach memory

That last point is what makes this a per-BLOCK cost, and why neither dead-flag elimination nor longer superblocks remove it. Only changing WHAT is stored does.

PROPOSAL. Store the operation and its operands instead of the six derived bits:

    cc_op   : u8   which operation produced the flags (ADD/SUB/LOGIC/SHL/INC/...)
    cc_src  : u64  operand
    cc_dst  : u64  result
    cc_size : (fold into cc_op)

Flag reads (eval_cond, setcc, cmov, adc/sbb, pushfq, and the interpreter) compute from that triple. Roughly 3 stores replacing ~35 instructions.

WHY THIS DOES NOT REGRESS INTRA-BLOCK CODE. cc_src/cc_dst are ordinary SSA values inside the block, so a `cmp` immediately followed by `jcc` folds back to a plain compare-and-branch in the mid-end — the same code Cranelift emits today for the fused case. The win is entirely at block boundaries, which is exactly where the cost was measured.

SCOPE AND RISK — this is a cross-cutting change and needs a written plan before any code:
  - the IR carries FlagMask per op (x86jit-core/src/ir.rs); cc_op must be derivable from the op + mask, including the x86 quirks the mask encodes (inc/dec keep CF; logic ops force CF=OF=0; shift-by-0 preserves everything — see TASK-224)
  - the INTERPRETER IS THE ORACLE. jit == interp is the project invariant. Either both engines move to the lazy representation, or every state-export point materializes. This decides whether CpuState's public shape changes, which is embedder-visible.
  - anything reading EFLAGS outside the two engines: the ELF/OCI embedders, the lockstep tracer (doc-32), unicorn differential validation, Exit paths that hand state back
  - undefined-flag cases must stay bit-identical to today or the fuzz seeds will fire

EXPECTED GAIN: ~45% fewer hot host instructions per block, against a workload measured as frontend-bound. Superseded in part by TASK-284 and TASK-285; measure from wherever those leave the number, not from 35.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 a written plan is reviewed and approved BEFORE implementation starts
- [ ] #2 TASK-285's result has been measured on the embedder and shows the frontend model transfers; otherwise this task is closed unimplemented with that result recorded
- [ ] #3 cc_op/cc_src/cc_dst representation is defined, including how it derives from IrOp + FlagMask and how it handles CF-preserving inc/dec, forced CF/OF, and shift-by-0
- [ ] #4 the jit == interp invariant holds; the decision on whether the interpreter also goes lazy is recorded with its reason
- [ ] #5 density_tests records the before/after chain-fixed cost; the reduction is stated as a number
- [ ] #6 cargo nextest run green including differential, unicorn and fuzz seeds; aarch64 cross-check clean
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
# Plan — lazy flags (Variant B)

DRAFT FOR REVIEW. No code until this is approved AND the embedder has re-measured
TASK-285.

## 1. Where the number stands

`density_tests`, `alu reg,reg`, host instructions in a block that exits by chaining:

    baseline (before TASK-284)   56.0
    after TASK-284               50.7   -9.5%   (constant-pool masks removed)
    after TASK-285               40.3  -20.5%   (AF/PF stored as sources)

At the embedder's 2.9 guest instructions per block that is ~62 -> ~47 host
instructions per block. Roughly 22 of the remaining 40.3 are still flags: CF, ZF,
SF and OF are each computed and stored on the last flag-setting instruction of the
block, because those flags are live across the block boundary.

Lazy flags target exactly that 22.

## 2. The mechanism

Store what produced the flags instead of the flags:

    cc_op:  u32   operation + operand size, or CC_OP_STATIC
    cc_a:   u64   first operand   (masked to size)
    cc_b:   u64   second operand  (masked to size)
    cc_res: u64   result          (masked to size)

Four stores plus one constant materialization — about 5-6 host instructions —
replacing ~22. `CC_OP_STATIC` means "the six flag bytes are authoritative", which
is what `popf`/`sahf`/the interpreter/an embedder write.

## 3. The question that decides whether this works at all

**How often is a flag read in a DIFFERENT block from the one that set it?**

Inside a block nothing changes: the lifter knows statically which op set the flags,
so `cmp` + `jcc` still lowers to a compare and a branch, and the mid-end
dead-store-eliminates the `cc_*` stores when a later instruction in the same block
overwrites them. The whole cost and the whole benefit sit at the block boundary.

- If cross-block flag reads are RARE, the stored `cc_*` are usually never read and
  we have replaced 22 instructions with 5. Large win.
- If they are COMMON, every such read must reconstruct the flags, and doing that
  inline means emitting a switch over `cc_op` — more code than we removed. Regression.

This is measurable statically, before writing any codegen: walk the lifted IR of a
real binary corpus and count, per block, whether the flags it leaves live are read
by a successor before being overwritten. **This measurement is step 0 and the plan
is abandoned if it comes out wrong.** Six direction choices in this workstream were
made ahead of measurement and all six missed.

## 4. If the measurement says cross-block reads are common

There is a fallback that fits the constraint we actually measured. The workload is
FRONTEND-bound: task-283 removed ~38 million helper calls per second from it with
zero effect on fps, which means calls are nearly free here and CODE SIZE is not.
So a cross-block flag read can be a single `call` into a Rust helper that
materializes the six bytes — one instruction at the call site instead of an inline
switch. Latency for a cold path, size for the hot one.

That is a real option, not a consolation prize, but it is a bet on the frontend
reading holding. It gets taken only if TASK-285 transfers to the embedder.

## 5. Phasing

    step 0  measure cross-block flag liveness over the ELF/OCI corpus. GO/NO-GO.
    step 1  add cc_op/cc_a/cc_b/cc_res to CpuState (appended; no existing offset
            moves). Interpreter sets cc_op = CC_OP_STATIC on every flag write —
            one store, interpreter semantics unchanged. Everything still eager.
            Gate: full suite green, density unchanged.
    step 2  JIT emits cc_* instead of the six flags for ONE op family (add/sub),
            with a helper for cross-block reads. Gate: jit == interp, unicorn
            differential, fuzz seeds, density measured.
    step 3  remaining families (logic, shifts, rotates, inc/dec, mul).
    step 4  materialize on the Exit boundary so `CpuState` stays valid for
            embedders and for the interpreter on tier-up handoff.

Steps 2 and 3 are individually revertible; step 1 is inert on its own.

## 6. What can go wrong

- **The oracle.** The interpreter validates the JIT. Keeping the interpreter eager
  (with CC_OP_STATIC) means the two representations must agree at every comparison
  point, so `Flags::PartialEq` compares DERIVED values — already true after
  TASK-285, and that is the mechanism this reuses.
- **Tier-up handoff.** A block may run interpreted then compiled. Every
  interpreter->JIT and JIT->interpreter transition must leave a consistent cc_op.
  Getting this wrong is silent and data-dependent; it needs a test that forces the
  handoff mid-block-chain.
- **The x86 quirks the FlagMask encodes** (inc/dec preserve CF; logic forces
  CF=OF=0; shift-by-0 preserves everything, see TASK-224; undefined flags must stay
  bit-identical or the fuzz seeds fire). cc_op has to encode enough to reproduce
  each of these, including the partial-mask cases where only some flags are written
  and the rest keep their previous cc_op — which is the subtle part: a partial
  update means the state is no longer describable by a SINGLE cc_op. Simplest
  correct answer is to materialize the untouched flags before switching cc_op;
  whether that eats the win on `inc`/`dec` (very common) is itself a step-0
  question.
- **Embedder-visible state.** `CpuState` is public and `#[repr(C)]`. Nothing may
  read a stale flag byte. Step 4 exists for this and must not be deferred.

## 7. Expected result

If step 0 says go: ~22 of 40.3 host instructions per block become ~6, i.e. another
~40% off the block, ~55% off the original baseline. On the embedder's numbers that
is the difference between ~21x and ~9x expansion — but the transfer to fps is
exactly what TASK-285 is being measured for, and this plan does not start until
that answer is in.
<!-- SECTION:PLAN:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
