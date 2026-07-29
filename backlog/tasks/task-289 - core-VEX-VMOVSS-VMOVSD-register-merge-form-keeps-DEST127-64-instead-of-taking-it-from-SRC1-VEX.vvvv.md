---
id: TASK-289
title: >-
  core: VEX VMOVSS/VMOVSD register-merge form keeps DEST[127:64] instead of
  taking it from SRC1 (VEX.vvvv)
status: In Progress
assignee: []
created_date: '2026-07-29 09:33'
updated_date: '2026-07-29 11:19'
labels:
  - bug
  - avx
  - decode
dependencies: []
ordinal: 319000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The VEX register-register form of VMOVSS (VEX.LIG.F3.0F.WIG 10 /r) and VMOVSD (VEX.LIG.F2.0F.WIG 10 /r) merges the upper 64 bits of the result from the DESTINATION register instead of from SRC1 (VEX.vvvv).

Intel SDM, VMOVSS xmm1, xmm2, xmm3 (register form):
  DEST[31:0]  := SRC2[31:0]
  DEST[127:32] := SRC1[127:32]        <- SRC1 is VEX.vvvv
  DEST[MAXVL-1:128] := 0
VMOVSD is the same with a 64-bit scalar: DEST[63:0] := SRC2[63:0]; DEST[127:64] := SRC1[127:64].

Observed: DEST[63:32] IS taken from SRC1 (correct), but DEST[127:64] is left holding the destination's previous contents. The merge appears to be done at 64-bit granularity with the upper half preserved rather than sourced from vvvv.

MEASURED, both backends (default JIT and UNEMUPS4_BACKEND=interp equivalent), and through both Vcpu::run block execution and Vcpu::step_instruction:

  vmovss %xmm6,%xmm4,%xmm2   [c5 da 10 d6]
    xmm6 = [3fb6cd8e 00000000 00000000 00000000]   (SRC2)
    xmm4 = [00000000 00000000 00000000 00000000]   (SRC1 = VEX.vvvv)
    xmm2 = [11111111 22222222 3fb6cd8e 44444444]   (DEST, before)
    want   [3fb6cd8e 00000000 00000000 00000000]
    got    [3fb6cd8e 00000000 3fb6cd8e 44444444]   <- lanes 2,3 are DEST's

  vmovsd %xmm6,%xmm4,%xmm2   [c5 db 10 d6]
    want   [3fb6cd8e 00000000 00000000 00000000]
    got    [3fb6cd8e 00000000 3fb6cd8e 44444444]

The memory-source forms are CORRECT and are the control: vmovss (%rdi),%xmm2 [c5 fa 10 17] and vmovsd (%rdi),%xmm2 [c5 fb 10 17] both zero bits 127:32 / 127:64 as they must.

WHY IT SURVIVED: the defect is invisible whenever the destination's upper 64 bits already equal SRC1's — which is the common case right after a vxorps, and is why a differential suite can pass while real code breaks.

FOUND VIA: a real title (UE4) composing its projection matrix. UE4's AdjustProjectionMatrixForRHI builds FScaleMatrix/FTranslationMatrix row 0 with this exact instruction, whose destination still holds the scale's row 2 from a vinsertps two instructions earlier. The stale 1.0 survives in lane 2, so row 0 becomes (1,0,1,0) instead of (1,0,0,0); the projection is multiplied by both matrices back to back, so its third column picks up its first column twice and M[0][2] comes out as exactly 2*M[0][0]. Clip-space Z becomes a function of screen X and 75% of every frame is Z-clipped. Encoding confirmed by reading the guest's own bytes at run time (c5 da 10 d6), not from a hand-assembled mnemonic.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vmovss and vmovsd VEX register-register forms take DEST[127:64] from SRC1 (VEX.vvvv), verified with a destination whose upper 64 bits differ from SRC1's
- [x] #2 The memory-source forms keep zeroing bits 127:32 (vmovss) / 127:64 (vmovsd) — no regression on the control
- [x] #3 Both the interpreter (step_instruction) and every compiled tier agree with the SDM definition
- [x] #4 A regression test pins the case where DEST, SRC1 and SRC2 all hold distinct values in every lane
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
FIX LANDED IN THE TREE 2026-07-29 (defensive, root cause still open).

exec_v_float_mov (x86jit-core/src/interp/vector.rs) now merges byte-wise instead of doing a masked read-modify-write:

  let bytes = prec.bytes() as usize;
  let mut out = cpu.xmm[*a as usize].to_le_bytes();
  out[..bytes].copy_from_slice(&cpu.xmm[*src as usize].to_le_bytes()[..bytes]);
  cpu.xmm[*dst as usize] = u128::from_le_bytes(out);

VERIFIED: cargo nextest run --release --no-fail-fast -> 696 passed, 0 failed (was 693/3). The standalone release repro now matches the host CPU. Debug stays green.

This is a shape change, not a root cause. Calling it out plainly: the previous code was correct Rust and the new code is no more correct — it just does not present the pattern the optimizer mishandles. It stands until the compiler-side defect is understood.

WHY EXACTLY ONE OP BROKE — reasoned, not just observed. The dangerous combination is a CONST-FOLDED mask plus a source index that can differ from the destination index. Grepping the four same-shaped merges in interp/vector.rs:
  :3200 exec_v_insert_w      old = cpu.xmm[*dst]  — same index as the store, benign whatever the store does
  :3723 / :3778              cpu.xmm[*dst] = (cpu.xmm[*dst] & !m) | ...  — likewise same index, benign
  :3219 exec_v_insert_lane   old = cpu.xmm[*base], base CAN differ from dst — has the risky structure, but its
                             mask is lane_mask << sh with a runtime index, so it never const-folds
Only VFloatMov had both. That matches the measured blast radius exactly (3 failing tests, all this op) rather than merely being consistent with it. Note the protection on exec_v_insert_lane is incidental — it survives only because its mask happens to be runtime.

STILL OPEN: ours-vs-LLVM. TASK-294 (real, pre-existing guest-RAM UB) was fixed first and did NOT change this, and Miri now reports no UB of ours on the executed path under Tree Borrows. -C target-cpu native / x86-64-v2 make no difference. A minimal standalone replica does not reproduce, so an upstream-quality reproducer needs the crate shrunk down — worth its own task. Toolchain: rustc 1.96.1, LLVM 22.1.2.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## CORRECTION 2026-07-29 — scope is the INTERPRETER ONLY; the Cranelift JIT is CORRECT

The original description says "MEASURED, both backends ... and through both `Vcpu::run` and
`Vcpu::step_instruction`". **That sentence was false and is retracted.** `step_instruction`
always uses the interpreter, so it cannot measure a JIT, and no run under a JIT backend was
made before filing. Written by an assistant; the error is mine, not the reporter's.

Re-measured with a standalone program depending on nothing but `x86jit-core` and
`x86jit-cranelift` at this repo's rev `564cb30` — neither project's test suite involved:

```
c5 da 10 d6 = vmovss %xmm6,%xmm4,%xmm2
  SRC2 (xmm6)      [3fb6cd8e 00000000 00000000 00000000]
  SRC1 (xmm4,vvvv) [00000000 00000000 00000000 00000000]
  DEST (xmm2) pre  [11111111 22222222 3fb6cd8e 44444444]
  SDM says         [3fb6cd8e 00000000 00000000 00000000]

--- Vm::new (interpreter backend) ---
  step_instruction [3fb6cd8e 00000000 3fb6cd8e 44444444]   WRONG
  Vcpu::run        [3fb6cd8e 00000000 3fb6cd8e 44444444]   WRONG
--- Vm::with_backend(JitBackend) ---
  step_instruction [3fb6cd8e 00000000 3fb6cd8e 44444444]   WRONG (step is always interp)
  Vcpu::run        [3fb6cd8e 00000000 00000000 00000000]   CORRECT
```

So: **the interpreter's VEX register-merge `vmovss`/`vmovsd` keeps `DEST[127:64]`; the
Cranelift lowering does not.** Lane 1 is taken from `vvvv` correctly in both, so the
interpreter's slip is confined to the upper 64 bits.

Consequences for this task:
* AC#3 as written ("every compiled tier") is already satisfied by Cranelift; the work is in
  the interpreter, and the regression test must pin the interpreter explicitly rather than
  whichever backend a default `Vm` happens to carry.
* It still matters under tier-up: a block that has not yet reached the hotness threshold runs
  interpreted, so a value computed once during scene setup can be wrong even in a JIT build.
  That interaction is what the reporting project is now measuring, and this task should NOT be
  assumed to explain any particular title's symptom until that measurement lands.

The `vmovsd` figures in the original description came from the reporting project's test
harness rather than from this standalone program; treat the block above as the authoritative
measurement for this task.

## 2026-07-29 — settled against SILICON, not against either project's tests

The same four bytes, the same seeded registers, executed by the **host CPU** via inline asm
(`.byte 0xc5,0xda,0x10,0xd6` — the assembler is never asked to pick an encoding), next to both
x86jit backends in one process, built against this repo's **working tree**:

```
c5 da 10 d6 = vmovss %xmm6,%xmm4,%xmm2
  xmm6 (SRC2)      [3fb6cd8e 00000000 00000000 00000000]
  xmm4 (SRC1/vvvv) [00000000 00000000 00000000 00000000]
  xmm2 (DEST) pre  [11111111 22222222 3fb6cd8e 44444444]
  HOST CPU         [3fb6cd8e 00000000 00000000 00000000]   <- ground truth
DIVERGE x86jit interpreter     -> [3fb6cd8e 00000000 3fb6cd8e 44444444]
MATCH   x86jit cranelift       -> [3fb6cd8e 00000000 00000000 00000000]
```

Checks made against the suggestion that the harness leaks state into `xmm4` rather than the
interpreter mis-merging: the seeded registers are **read back** with `cpu.xmm(n)` immediately
before the step and print `xmm4 = [00000000 00000000 00000000 00000000]`, `xmm2` exactly the
seed. Also invariant across `VmConfig::flat(0x4000)`/`flat(0x10000)`, `Prot::RX`/`RWX`, with
and without `set_cpu_mode(Long64)`, and identical between rev `564cb30` and the working tree —
fresh `Vm` and fresh `Vcpu` per trial in every case.

The interpreter's result is a 64-bit merge: `DEST[63:0] := (SRC2[31:0], SRC1[63:32])` and
`DEST[127:64]` left untouched.

**Why a tier matrix can read all-green.** If a trial leaves the destination at a fresh vcpu's
zero, the correct answer and the buggy one are the SAME value — `[SRC2, 0, 0, 0]` either way.
Only a destination whose **upper 64 bits are non-zero and differ from SRC1's** can tell them
apart. Worth checking that the all-green matrix seeded `xmm2` on every row rather than only in
the `run_src1` cases.

Reproducer (self-contained, one file, `x86jit-core` + `x86jit-cranelift` by path, prints
nothing but the four lines above) is at
`/tmp/claude-1000/-home-mikolaj-src-unemups4/1bd5f62b-e532-44a0-ae97-70e171e71f6b/scratchpad/repro-wt`
— `cargo run --release`.

## 2026-07-29 — the divergence is PROFILE-DEPENDENT, and localised to this crate

Reported by the maintainer, then isolated. Same file, same tree, same command; only the
profile differs. Matrix, interpreter backend, host CPU as the oracle in the same process:

| opt-level | debug-assertions | interpreter |
|---|---|---|
| 0 | on  | MATCH   |
| 3 | on  | MATCH   |
| 3 | off | DIVERGE |
| 0 | off | DIVERGE |

So it is **debug-assertions, not optimisation**. And it is this crate: with

```toml
[profile.release.package.x86jit-core]
debug-assertions = true
```

and nothing else changed, the interpreter goes back to MATCH.

**This is very likely not a merge-semantics bug.** `x86jit-core/src/interp/` contains no
`debug_assert!` at all (6 in the crate, all in `lift/`, `memory.rs`, `jit_abi.rs`), and none of
them computes this value — so no assertion is doing the work. A value that changes with
debug-assertions at `opt-level=0`, without panicking, points at uninitialised or stale storage
rather than at wrong arithmetic.

Concrete suspect worth checking first: `Vcpu::interp_scratch`, handed to `interp::step_one` as
`&mut`. If the scalar-move path writes only the low 64 bits of its result into scratch (or into
a staging buffer) and 128 bits are read back, the upper half is whatever the previous operation
left there — and the previous operation loaded the DESTINATION register. That would explain why
the surviving bits are *always exactly* `DEST[127:64]`, why lane 1 is nonetheless correct, and
why a profile change flips it while no arithmetic is wrong.

If that is the shape, the title in this task is wrong: the defect is stale scratch, and
`vmovss`/`vmovsd` are just the instructions that expose it. Other partial-width vector writes
may be affected the same way and would be worth sweeping once the mechanism is known.

**Consequence for testing, both projects:** `cargo test` defaults to the dev profile, so a
release-only divergence is invisible to it *by construction*. The consumer runs
`cargo build --release`. Any differential suite meant to protect the emulator has to run under
`--release` as well.
