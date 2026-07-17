---
id: TASK-269
title: >-
  BUG: legacy packsswb/packssdw zero the ymm upper half (should preserve) —
  exec_vpack uses set_vec on a shared VEX IR op
status: In Progress
assignee: []
created_date: '2026-07-17 19:19'
updated_date: '2026-07-17 20:01'
labels:
  - bug
  - simd
  - fuzz
dependencies:
  - TASK-266
ordinal: 299000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by TASK-266 (legacy-SSE upper-half audit, empirical native-oracle probe x86jit-tests/tests/legacy_upper_audit.rs). Of the 62 legacy-SSE ops probed with a pre-dirtied ymm upper, the real host CPU preserved bits 255:128 on all 62; the interpreter WRONGLY ZEROED them on exactly two instructions: the signed packs **packsswb** and **packssdw**. (The unsigned pack packuswb and all 40 packed-integer / logic / unpack / minmax / sat / avg / pmaddwd ops correctly preserve.)

Root cause (verified statically): legacy packsswb/packssdw lift via lift_pack_signed (x86jit-core/src/lift/vector.rs:3306) to IrOp::VPackWide { bytes: 16 }, whose interp exec exec_vpack (x86jit-core/src/interp/mod.rs:4744) ends with:

    cpu.set_vec(dst as usize, res, bytes);   // set_vec ZEROES bits above

With bytes=16 that zeroes ymm_hi — wrong for a legacy SSE instruction, which must preserve the upper. jit==interp holds (the JIT shares the same helper), so the JIT is wrong the same way; only the native oracle caught it.

The subtlety — VPackWide is shared THREE ways with conflicting upper rules, so exec_vpack cannot be keyed on  alone:
  - legacy packsswb/ssdw : bytes=16, must PRESERVE 255:128   (currently clears — BUG)
  - VEX.128 vpack*        : bytes=16, must CLEAR 255:128      (currently clears via set_vec — correct)
  - VEX.256 vpack*        : bytes=32, must CLEAR 511:256      (currently clears — correct)
legacy and VEX.128 both land on bytes=16 but need OPPOSITE behavior. So the clear/preserve decision must move to the LIFT, which knows the encoding — the established task-262 pattern (see lift_vpblendw / lift_byteshift_avx: exec uses set_vec_low to preserve, and the VEX lift appends a trailing VZeroUpper to clear).

In-file precedents that are already correct and show the shape:
  - The MEMORY form: lift_vpack reg-vs-mem split (lift/vector.rs:2401-2414) already emits VPackWideM + an explicit VZeroUpper, and pack_wide_mem (interp/mod.rs:4764) writes cpu.xmm[dst] directly (preserves). Only the REGISTER form regressed.
  - exec_pmaddwd (interp/mod.rs:4771) writes cpu.xmm[dst] directly with the comment 'Legacy SSE: preserves bits 255:128'.

Suggested fix:
  1. exec_vpack: set_vec -> set_vec_low (preserve the upper for the register form).
  2. lift_vpack (VEX register path, after the VPackWide push at ~2417-2424): append IrOp::VZeroUpper { reg: dst } when bytes == 16 (VEX.128). For bytes == 32 (VEX.256) the semantics are clear-511:256; verify set_vec_low(32) plus the existing zmm handling still yields that (under v3 zmm_hi is always 0, but keep it correct for AVX-512 state — a bytes=32 write must still clear 511:256, so the ymm form likely also needs an explicit upper-clear rather than relying on set_vec_low).
  3. lift_pack_signed (legacy): unchanged — with exec preserving, the legacy form is now correct with no VZeroUpper.
  4. Mirror the fix for the JIT helper if it has its own copy of the pack writeback (grep the cranelift pack helper), so jit==interp is maintained.

Reproduce: cargo test --release -p x86jit-tests --test legacy_upper_audit -- --ignored --nocapture  (packsswb / packssdw rows show interp cleared, native preserved).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Legacy packsswb and packssdw preserve bits 255:128 of the destination (verified: legacy_upper_audit reports OK for both, native and interp agree)
- [ ] #2 VEX.128 vpacksswb/vpackssdw still CLEAR bits 255:128, and VEX.256 still clears 511:256 — no regression, proven by a native-vs-interp test covering both VEX widths with a pre-dirtied upper
- [ ] #3 jit == interp for all pack forms (legacy/VEX.128/VEX.256, register and memory src2)
- [ ] #4 A native-vs-interp regression test in x86jit-tests/src/native.rs drives legacy packsswb/packssdw with a dirty ymm upper and is proven to FAIL without the fix
- [ ] #5 cargo nextest run -E 'not binary(fuzz_robustness)' and cargo clippy --all-targets --all-features -- -D warnings both pass
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed by agent, validated in combined gate. exec_vpack set_vec->set_vec_low; lift_vpack appends VZeroUpper only for VEX.128 (bytes==16); legacy path unchanged. Native test native_pack_signed_upper_half_preserve_vs_clear_match_interp proven RED without fix. Re-ran the legacy_upper_audit probe post-fix: OK=62, BUG=0 (packsswb/packssdw now preserve). No cranelift edit (vpack_helper calls shared exec_vpack). Combined gate green. Ready for review; not committed.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
