---
id: TASK-270
title: >-
  BUG: SAR CF wrong for shift count >= operand width on sub-64-bit operands
  (exec_sar missing sign-extension)
status: In Progress
assignee: []
created_date: '2026-07-17 20:32'
updated_date: '2026-07-17 21:14'
labels:
  - bug
  - fuzz
dependencies: []
ordinal: 300000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the AVX fuzz campaign (TASK-264) after the has_legacy_vec filter was removed (TASK-266) — a bystander in a mixed program, shrunk to a base-GPR shift; not a vector bug.

exec_sar (x86jit-core/src/interp/integer.rs:261) computes the carry flag as:

    let cf = (vm >> (cnt - 1)) & 1 != 0;

vm is the operand masked to its width (mask(size)). For a sub-64-bit SAR whose masked count (shift_mask: 31 for 8/16/32-bit, 63 for 64-bit) is >= the operand width, cnt-1 exceeds the top bit of vm, so vm >> (cnt-1) is 0 and CF is wrongly cleared. For an ARITHMETIC right shift the operand is conceptually sign-extended, so the last bit shifted out at those counts is the SIGN bit. Hardware sets CF = sign bit; interp sets CF = 0.

Witness (cargo xfuzz --seed 219): shrinks to `sar r16, 31` — count masks to 31, width 16. Native CF=true (sign set), interp CF=false. native-vs-interp divergence on CF only.

Note SHR (exec_shr) uses the same `vm >> (cnt-1)` shape and is CORRECT there — a logical shift feeds in zeros, so CF=0 at cnt>width is right. Only SAR needs the sign-extended operand.

Fix (one line): use the sign-extended value for the CF bit, mirroring the result computation on line 258 which already does `sign_extend(vm, *size) as i64 >> cnt`:

    let cf = (sign_extend(vm, *size) >> (cnt - 1)) & 1 != 0;

(cnt is masked to <=63 and cnt>=1 in this branch, so cnt-1 <= 62 — no overflow.)

Also verify the JIT path: the fuzzer flagged native-vs-interp; check whether the JIT computes SAR CF via a shared helper (then jit==interp, both wrong, fixed by this change) or via the host sar instruction (then jit is already correct and this fix also removes a latent jit-vs-interp divergence). Keep jit==interp.

Reproduce: cargo run --release -p x86jit-tests --bin fuzz -- --seed 219
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 exec_sar sets CF = sign bit of the operand when the masked count >= operand width (SAR r16,31 with sign set → CF=1), matching the NativeOracle across 8/16/32/64-bit widths and counts spanning 1..width..mask
- [ ] #2 SHR/SHL/rotate CF behavior is unchanged (no regression) — only SAR touched
- [ ] #3 jit == interp for SAR CF across the same width/count matrix
- [ ] #4 A native-vs-interp regression test drives sar across widths and over-width counts and is proven to FAIL without the fix
- [ ] #5 cargo nextest run -E 'not binary(fuzz_robustness)' and clippy -D warnings pass
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed in interp AND cranelift (both had the bug → jit==interp preserved). exec_sar (interp/integer.rs) and emit_shift Sar arm (cranelift codegen/mod.rs) now read CF from sign_extend(vm,size) instead of the width-masked vm. Native test native_sar_cf_overwidth_count_match_interp (8/16-bit SAR, over-width counts, per-CF setc capture + 32-bit control), proven RED without the interp fix. Witness seed 219 now clean on both legs. Ready for review; not committed.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
