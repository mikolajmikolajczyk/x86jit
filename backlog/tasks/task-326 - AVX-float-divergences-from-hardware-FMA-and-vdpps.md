---
id: TASK-326
title: 'AVX float divergences from hardware: FMA and vdpps'
status: In Progress
assignee: []
created_date: '2026-08-11 11:03'
updated_date: '2026-08-11 22:16'
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
- [ ] #2 FMA subnormal, inf-sign and NaN-quieting behaviour matches hardware, with the seeds above as witnesses
- [x] #3 Whether one root cause explains both is recorded either way
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#1 DONE, AC#3 ANSWERED, AC#2 ALL BUT ONE CASE.

ROOT CAUSE, and it is one cause for nearly all of it: the interpreter - the JIT's oracle - did not implement the architectural NaN rules. It inherited them from the host CPU and from LLVM. Rust and LLVM leave the NaN payload of a float op unspecified, and LLVM freely commutes commutative float ops, while the x86 rule is operand-order-dependent (SDM Vol 1 sec 4.8.3.5, Table 4-8: the result is the FIRST source operand, quieted if it is an SNaN). So `a * b` returned one operand's payload in a debug build and the other's in a release build OF THE SAME SOURCE. The same code on aarch64 would answer differently again, since ARM prefers an SNaN over the first operand.

That is why the divergence was invisible to a `cargo test` run: it only reproduced under --release. Fixed in 1b7dec2 in four places:
 - binary SSE/AVX ops (new sse_binop_f32), composed through dpps for each product and each node of the (P0+P1)+(P2+P3) tree;
 - one-operand ops (vround) - it went through f32 as f64 -> round -> as f32, and Rust specifies neither the payload nor the SNaN conversion across those casts;
 - FMA (fma_elem) - Q(x) else Q(y) else Q(z), same for every sign variant (SDM Vol 1 Table 14-17). Two bugs there: the neg_prod/neg_add flips were applied BEFORE the NaN check so the returned NaN came back sign-inverted, and the invalid-but-no-NaN cases (inf*0) returned an arbitrary NaN rather than the QNaN indefinite;
 - vcvtps2ph - a `.max(1)` forced bit 0 of the half "to keep it a NaN", but 0x7e00 already is one, so it corrupted the payload of every source NaN whose carried bits are zero.

The JIT's dpps_native got the same rule spelled out in Cranelift rather than relying on fmul/fadd, for the same reason. Both sides implementing it explicitly is what makes jit == interp hold on every host instead of by coincidence on this one.

MEASURED: `cargo xfuzz --secs 300` over 10811 VEX-bearing programs reports jit_hits=0, against 14 before. Every seed this task named is clean: 26816, 20980, and also 58, 223, 1432, 5915, 661, 19879, 22131, 22654, 25111.

TWO MORE DEFECTS FOUND WHILE HERE, both fixed:
 - 9cd2ace: `vpblendw dst, dst, [mem], imm8` mislifted. The memory form loads through dst and then blends src1 against it - sound only while dst is not also src1. lift_vmpsadbw guards the identical trick with `d != a`; this one did not. Wrong result, no trap, and invisible to jit-vs-interp because both tiers share the lift. Now rejected (the aliased form needs a temp vreg, a separate change) and it joins the reg_only list. Found by `cargo xfuzz --mem --seed 3130`.
 - 0a49c5f: the fuzz generator masked a shift count to the operand width instead of to 5 bits (6 for 64-bit operands, SDM Vol 2 SAL/SAR/SHL/SHR). `shl r8b, 16` masked to an effective count of 0, so the generator recorded the flags as untouched when the instruction really shifts by 16 and leaves AF undefined. That produced a FALSE finding (seed 13740) which would have kept reporting forever.

STILL OPEN - ONE CASE. `cargo xfuzz --seed 28739`, vfmsubadd213pd, native-vs-interp only (jit == interp). Native gives 0xffffffffffffffff in the high qword; we give 0x7ff80000ffffffff, which is quiet_f64(0x7ff00000ffffffff) - i.e. we quiet one source NaN where hardware returns a different one. The x->y->z precedence is now implemented; what is left to check is whether fma_lanes maps dst/src2/src3 onto the architectural x (multiplicand) / y (multiplier) / z (addend) correctly for the 213 form and for the alternating sign variants of subadd. SDM Vol 1 Table 14-17 is the reference; the VFMSUBADD213PD page gives the operand roles (DEST[i]*SRC2[i] -/+ SRC3[i]).

Also note this seed is the ONLY finding in 10811 programs, so it is narrow.

TESTS. interp::nan_rule_tests pins all four rules BY VALUE - exact bit patterns, not is_nan(). That matters: a test that only checks the class passes against every wrong answer here, which is how this survived. RUN THEM IN RELEASE TOO; that is where it reproduced. Every fix was negative-controlled by breaking it and watching the test fail, except dpps_propagates_the_first_nan_in_sdm_tree_order, which is a composition and passed once with the rule removed - the test that actually pins the rule is sse_binary_ops_deliver_the_first_nan_source_operand, and its comment says so.

VERIFIED: 784 unit tests green in BOTH debug and release, clippy, fmt, aarch64 cross-check, compat map current, and the full 169-rung ladder.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
