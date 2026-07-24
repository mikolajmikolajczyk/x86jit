---
id: TASK-288
title: lift VPBLENDW (VEX.128 0x0e) — Little Nightmares UE4 (unemups4 consumer)
status: In Progress
assignee: []
created_date: '2026-07-24 05:30'
updated_date: '2026-07-24 05:39'
labels: []
dependencies: []
ordinal: 318000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
unemups4 running Little Nightmares (PS4 UE4 title) faults with UnknownInstruction: 'vpblendw $0x3f,0x10(%r12),%xmm13,%xmm9', bytes c4 43 11 0e 4c 24 10 3f (VEX.128.66.0F3A.WIG 0E /r ib = VPBLENDW xmm, xmm, xmm/m128, imm8 — per-word blend controlled by imm8). Guest RIP 0x310e2be on the main thread; blocks LN boot after the RHI throttle deadlock was cleared upstream. Consumer: unemups4 feat/ue4-little-nightmares. After landing, unemups4 bumps its x86jit rev pin.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 VPBLENDW VEX.128 0F3A 0E /r ib lifted: dst word[i] = (imm8>>i&1) ? src2.word[i] : src1.word[i], for i in 0..8
- [ ] #2 differential test vs a hardware/Unicorn oracle for representative imm8 masks incl 0x3f
- [ ] #3 memory operand form (0x10(%r12)) exercised
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-24. AC#1-#3 met; embedder-side rev pin is theirs.

VPBLENDW was already lifted (lift_vpblendw -> IrOp::VBlendW, with interp exec_v_blend_w and JIT
emit_v_blend_w) — for a REGISTER src2 only. `vec_operand_reg(insn, 2)` returned None on the m128
form the title hits, so the lifter fell through to UnknownInstruction. This was purely the missing
memory-operand path, not a missing op.

FIX (x86jit-core/src/lift/vector.rs, lift_vpblendw): on an m128 src2, load it into `dst` via
IrOp::VLoad{size:16} and blend with `b = dst`. Sound because both exec_v_blend_w and emit_v_blend_w
read `a` and `b` fully before writing `dst` (verified in source), so aliasing dst onto src2 loses
nothing and needs no temp vreg — the same shape lift_vpshufd already uses for its memory form.
Added the `tg: &mut TempGen` param for effective_address; updated the one caller in lift/mod.rs.
No IR-op, interp or codegen change.

COVERAGE:
  - x86jit-tests/src/native.rs native_vpblendw_mem_matches_interp — the m128 form vs the REAL CPU
    (AC#2 + AC#3). Unicorn is not the oracle: its QEMU drops VEX vvvv, so src1 would be mis-decoded.
    Masks 0x3f (the reported one), 0x00, 0xff, 0x5a, each word distinct so a wrong per-word source
    diverges. Self-skips without AVX2.
  - x86jit-tests/tests/jit.rs vpblendw_mem_match_interp — jit == interp on the same masks, with
    ymm_hi seeded to prove VEX.128 upper-zeroing.
  Vpblendw was already in the coverage_ratchet ALLOWLIST (task-215), so no ratchet change.

Gates: cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' green; clippy
--all-targets --all-features -D warnings clean; fmt --check clean; cargo check --target
aarch64-unknown-linux-gnu --tests clean.

Committed separately from the uncommitted task-283 watch work also on the tree at the time (only the
lift + test files were staged).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
