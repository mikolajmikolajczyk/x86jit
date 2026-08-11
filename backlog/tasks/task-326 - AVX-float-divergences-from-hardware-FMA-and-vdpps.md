---
id: TASK-326
title: 'AVX float divergences from hardware: FMA and vdpps'
status: Done
assignee: []
created_date: '2026-08-11 11:03'
updated_date: '2026-08-11 23:13'
labels: []
dependencies: []
priority: medium
ordinal: 362000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Both surfaced from the same fuzz campaign once task-205 made NaN-payload tolerance safe, and both are native-vs-interp — so the softfloat interpreter, the JIT's oracle, is itself wrong.

**FMA** (vfmaddsub/vfmsubadd/vfmadd): a subnormal f32 result divergence (0x05f8 vs 0x0678 — double rounding or an unfused a*b+c), a large finite sign divergence on vfmsubadd213ps, and a ±inf lane flip on the pd forms. Not NaN noise; real arithmetic.

**vdpps** diverges on two axes at once — jit-vs-interp (seed 26816, which violates the hard invariant) and native-vs-interp (seeds 20980 and 26816).

Kept together because they are one investigation: both are dot-product/fused-multiply paths through the same float helpers, both were found by the same tool, and the vdpps jit-vs-interp axis is the loose thread most likely to explain the rest. Merged from task-206 and task-211.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vdpps agrees jit-vs-interp — the hard invariant is restored first
- [x] #2 FMA subnormal, inf-sign and NaN-quieting behaviour matches hardware, with the seeds above as witnesses
- [x] #3 Whether one root cause explains both is recorded either way
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
CLOSED. All three acceptance criteria met.

ONE ROOT CAUSE explains everything this task listed. The interpreter - the JIT's oracle - did not implement the architectural NaN rules; it inherited them from the host CPU and from LLVM. Rust and LLVM leave the NaN payload of a float op unspecified and LLVM freely commutes commutative float ops, while the x86 rule is operand-order-dependent (SDM Vol 1 sec 4.8.3.5, Table 4-8: the result is the FIRST source operand, quieted if it is an SNaN). So `a * b` returned one operand's payload in a debug build and the other's in a release build OF THE SAME SOURCE - which is why nothing under `cargo test` ever saw it.

FIXED IN THREE COMMITS.

1b7dec2 - the four sites the campaign reached: binary SSE/AVX ops (new sse_binop_f32/f64), one-operand ops (vround), FMA (fma_elem), and vcvtps2ph. FMA had two distinct bugs beyond the payload: the neg_prod/neg_add sign flips were applied BEFORE the NaN check so the returned NaN came back sign-inverted, and the invalid-but-no-NaN cases (inf*0) returned an arbitrary NaN rather than the QNaN indefinite. vcvtps2ph forced bit 0 with `.max(1)` "to keep it a NaN", corrupting every source NaN whose carried bits are zero.

9cd2ace - `vpblendw dst, dst, [mem], imm8` mislifted (the memory form loads through dst, destroying src1 when they alias; lift_vmpsadbw guards the identical trick, this one did not). Now rejected.

0a49c5f - the fuzz generator masked a shift count to the operand width instead of to 5 bits, producing a FALSE finding on undefined AF.

ca55bbc - the audit pass. The first commit fixed what the campaign happened to reach; an audit of every float expression in x86jit-core/src/interp/ found fourteen more of the same defect. Binary arithmetic (apply_f32/apply_f64) is by far the largest blast radius - every SSE and AVX add/sub/mul/div. sqrt/rsqrt/rcp needed the unary rule plus the QNaN indefinite for a negative operand. dppd was the twin of dpps, left on bare * and +. The four float-to-float converts needed the payload carried by 29 bits with the quiet bit set, which was MEASURED on hardware through the native oracle rather than assumed.

MIN/MAX DELIBERATELY NOT CHANGED, and there is now a test that fails if someone sweeps them in. Their rule is the opposite one and was already correct: "if only one value is a NaN (SNaN or QNaN) for this instruction, the second operand (source operand) ... is written to the result", and "if a value in the second operand is an SNaN, then SNaN is forwarded unchanged ... a QNaN version of the SNaN is not returned" (SDM Vol 2 MINPS/MAXPS). `if x < y { x } else { y }` with x = SRC1 gives exactly that.

THE JIT got the rule for its binary ops too. AArch64's FPProcessNaN prefers a signalling NaN over the first operand, so it disagrees with x86 in exactly one case (SRC1 quiet, SRC2 signalling); when SRC1 is not a NaN the rules coincide, so guarding SRC1 alone closes the gap. Emitted on EVERY host rather than cfg-gated on aarch64: a cfg-gated version could not be executed before release at all, since this repo builds on x86 and Cranelift links only the host ISA backend. Ungating found two real bugs in that code within one test run. Perf gate clean.

MEASURED RESULT: `cargo xfuzz --secs 300` over 10699 VEX-bearing programs reports zero divergences on both axes - jit_hits=0 AND native_hits=0. This task opened with a jit-vs-interp hard-invariant violation plus FMA and vdpps native divergences.

The memory-operand leg (`cargo xfuzz --mem`) still reports 8 findings, all confirmed to be `UnknownInstruction` traps rather than wrong results - they are the unlifted memory forms the coverage map lists as reg_only, which is that leg's documented expected output, not a regression.

HONEST LIMIT ON THE TESTS, stated in the module doc so nobody over-reads them. On an x86 host with the arithmetic actually executed, Rust's `*` compiles to `mulss`, whose NaN rule IS this rule - so removing the fix from apply_f32, apply_un_f32 or dppd and re-running still passes. Checked, by doing exactly that. What the tests pin is drift on a host whose rule differs (aarch64) and drift when the optimizer folds or reassociates instead of emitting the instruction. float_to_float_conversion_carries_the_nan_payload is the exception - a Rust `as` cast canonicalizes even on x86 - and it does fail against the unfixed code. The tool that catches the rest is `cargo xfuzz` in release against the native oracle.

VERIFIED: 789 unit tests green in BOTH debug and release, clippy, fmt, aarch64 cross-check, compat map current, perf gate clean, and the full 169-rung ladder.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
