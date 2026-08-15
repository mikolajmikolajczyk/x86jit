---
id: TASK-333
title: 'lift: masked EVEX forms can silently drop the write mask'
status: To Do
assignee: []
created_date: '2026-08-15 10:04'
updated_date: '2026-08-15 10:04'
labels:
  - m8-simd
dependencies: []
priority: high
ordinal: 369000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A masked EVEX encoding whose mnemonic is dispatched by a VEX-era lifter that never checks `evex_is_masked` lifts as if unmasked. The guest gets a WRONG RESULT, not a trap — the destination lanes the mask should have preserved are overwritten.

Confirmed on `vpermilps`. `lift/mod.rs` dispatches on `insn.mnemonic()`, so `EVEX_Vpermilps_xmm_k1z_xmmm128b32_imm8` reaches `lift_vpermil_imm` (lift/vector.rs), which has no `evex_is_masked` guard while 18 sibling call sites in the same file do.

Reproducer (decode + lift, no execution needed) — bytes are EVEX.128.66.0F3A.W0 04 /r ib, `vpermilps xmm1{k1}, xmm2, 0x1b`:

    let code: &[u8] = &[0x62, 0xF3, 0x7D, 0x09, 0x04, 0xCA, 0x1B, 0xF4];
    // decoded: EVEX_Vpermilps_xmm_k1z_xmmm128b32_imm8 / Vpermilps mask=K1 zeroing=false
    // lift_block(...) -> Ok(5)   <-- should be Err(Unsupported)

Found during the pre-republish comment cleanup, by reading a doc comment that claimed EVEX was deferred here.

Note the two testing traps that hide this class: a masked-EVEX test built with iced's `code_asm` assembler is NOT masked (write the bytes by hand or set `Instruction::set_op_mask` and re-decode to confirm), and the ISA compat probe templates register operands without a mask, so it reports these forms as lifted.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Sweep every EVEX form that can carry an op-mask (`OpCodeInfo::can_use_op_mask_register`), encode it with k1, re-decode to confirm the mask survived the encoder, and lift it — producing the full list of mnemonics that lift a masked form without honouring the mask
- [ ] #2 Every mnemonic on that list either honours the mask or returns Unsupported; no mnemonic silently drops it
- [ ] #3 A regression test pins at least the vpermilps case by hand-written EVEX bytes, and fails before the fix
- [ ] #4 The compat probe reports masked forms honestly, or its blind spot to them is stated in the generated map the way the reg_only limit already is
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
