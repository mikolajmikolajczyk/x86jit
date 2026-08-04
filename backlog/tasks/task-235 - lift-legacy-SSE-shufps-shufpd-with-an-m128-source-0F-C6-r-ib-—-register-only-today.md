---
id: TASK-235
title: >-
 lift: legacy SSE shufps/shufpd with an m128 source (0F C6 /r ib) —
 register-only today
status: Done
assignee: []
created_date: '2026-08-02 20:50'
updated_date: '2026-08-02 21:15'
labels:
 - lift
 - sse
dependencies: []
ordinal: 331000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`lift_shufps` (x86jit-core/src/lift/vector.rs:2592) takes its second operand with `reg_xmm(insn, 1).ok_or_else(|| unsupported_insn(insn))?`, so the memory-source form traps as UnknownInstruction. This covers BOTH mnemonics — lift/mod.rs:1223 routes `Shufps | Shufpd => lift_shufps`, so `shufps xmm, m128, imm8` (0F C6 /r ib) and `shufpd xmm, m128, imm8` (66 0F C6 /r ib) are equally affected.

Lift-side gap only, exactly like TASK-208/296 (vextractf128 memory destination). Everything downstream already exists and is exercised:
- `IrOp::VShufpsM` (ir.rs:759),
- interp `exec_v_shufps_m` (interp/vector.rs:3087),
- Cranelift `emit_v_shufps_m` (codegen/vector.rs:3336, dispatched at codegen/mod.rs:1476).

They exist because TASK-191 lifted the VEX form: `lift_vshufps` (vector.rs:~2670) already routes a memory src2 to `VShufpsM` via the `vec_src_dispatch!` macro. The legacy path simply never got the same treatment. So the fix is to give `lift_shufps` the same `vec_src_dispatch!` shape, with `a = d` (legacy is two-operand, dst is also the merge base).

UPPER-HALF SEMANTICS — the trap to get right. Legacy SSE writes only bits 127:0 and PRESERVES 255:128; only the VEX encodings zero the upper (conventions.md, spec.md 16). `VShufpsM` is a SHARED op: the VEX path gets its zeroing from a trailing `IrOp::VZeroUpper` appended after the dispatch, not from the op itself. Checked both tiers and they already preserve — `exec_v_shufps_m` writes `cpu.xmm[dst]` and the JIT uses `store_xmm`, neither touching the upper — so routing the legacy form here is safe and must NOT append VZeroUpper. This is nevertheless the exact bug class TASK-200 audits and TASK-203 records (packsswb/packssdw zeroing the ymm upper through a shared VEX IR op), so it needs a test with a pre-dirtied ymm upper rather than an assumption.

Note the register form is in-place (dst is both destination and first source) while the VEX form has a distinct merge base, and `exec_v_shufps_m` deliberately reads the merge base before writing dst so aliasing is safe — which is what the legacy dst==a case relies on.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 shufps xmm, m128, imm8 and shufpd xmm, m128, imm8 lift to VShufpsM instead of returning unsupported_insn
- [x] #2 The legacy memory form PRESERVES ymm bits 255:128, proven by a test with a pre-dirtied upper half, and no VZeroUpper is appended on this path
- [x] #3 Encodings pinned by an llvm-mc or objdump witness
- [x] #4 jit==interp coverage for both mnemonics in the memory-source form, plus native bit-exact validation against the real CPU
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented 2026-08-02. Lift-side only, mirroring what task-191 already did for the VEX form: lift_shufps now routes operand 1 through vec_src_dispatch! with a = d (legacy is two-operand, so dst doubles as the merge base), emitting VShufps for a register source and VShufpsM for m128. IrOp::VShufpsM, exec_v_shufps_m and emit_v_shufps_m all pre-existed; the VEX path is byte-identical. NO trailing VZeroUpper on this path - legacy SSE preserves ymm 255:128 and only the VEX encodings zero it. Encodings witnessed with llvm-mc and objdump: 0f c6 08 1b, 0f c6 5f 10 c9, 0f c6 08 e4 (shufps) and 66 0f c6 10 01, 66 0f c6 6e 20 02, 66 0f c6 10 03 (shufpd). imm8 coverage: shufps 0x1B/0xC9/0xE4/0x00, shufpd all four, plus both register forms as regression. Upper half proven preserved with per-register sentinels (ymm_hi[r] = 0xDEADBEEF...0001 ^ r) surviving on real hardware, while a VEX vshufps in the same program correctly zeroes its own. TWO NEGATIVE CONTROLS, both re-run independently on main and not merely reported: reverting the lift to register-only fails the test with UnknownInstruction{addr:4115} vs Hlt, and injecting a stray VZeroUpper fails it with 'legacy SSE must PRESERVE ymm3 bits 255:128, left: 0'. METHOD FINDING worth carrying forward: jit_eq_interp alone cannot prove a lift exists, because an unlifted opcode traps identically in BOTH tiers - the agent's first draft of this test passed with the fix reverted. The final test runs both engines explicitly, asserts ExitKind::Hlt, and compares against an independently derived SDM reference (shufpd computed as two 64-bit lane picks rather than via the lift's dword expansion). COMPAT FINDING: the map was over-reporting before this change. compat.rs synthesizes *_or_mem operand kinds as the REGISTER alternative, so Shufps_xmm_xmmm128_imm8 was probed as a register shuffle and scored lifted while the m128 form faulted. This is structural and not shufps-specific: no lift-side memory-form gap is visible to the map for any *_or_mem operand. Recorded in status.md. Gates on the merged main tree: 921 passed / 8 skipped, clippy clean, fmt clean, aarch64 check clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
