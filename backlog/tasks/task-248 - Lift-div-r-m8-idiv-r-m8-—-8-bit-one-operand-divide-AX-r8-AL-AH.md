---
id: TASK-248
title: 'Lift div r/m8 idiv r/m8 — 8-bit one-operand divide (AX / r8 -> AL:AH)'
status: Done
assignee: []
created_date: '2026-07-15 09:36'
updated_date: '2026-07-15 09:44'
labels:
  - lift
dependencies: []
ordinal: 278000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Retail bring-up (unemups4 Celeste) hit an unlifted 8-bit DIV during boot: div %dil (bytes around 40 F6 F7, REX to access DIL). DIV r/m8 (F6 /6) and IDIV r/m8 (F6 /7) divide the 16-bit AX by the r/m8 divisor, packing quotient->AL and remainder->AH — distinct from the 16/32/64-bit forms that split RDX:RAX into two registers. lift_div rejected size<2, so both 8-bit forms were unlifted. (MUL/IMUL r/m8 were already done in task-189.) The Div IR op + interp divide() + cranelift div helper already support size=1; only the lift needed the AX-dividend / AL:AH-result wiring. Acceptance: div r/m8 + idiv r/m8 lift + execute interp == jit == unicorn (normal, signed-negative, div-by-zero #DE, quotient overflow, DIL via REX).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 div r/m8 and idiv r/m8 lift and match Unicorn (normal + signed + edge)
- [x] #2 div-by-zero and quotient-overflow raise #DE (vector 0) on both tiers
- [x] #3 jit == interp for the 8-bit div forms
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-15. MUL/IMUL r/m8 were already lifted (task-189); only DIV/IDIV r/m8 were missing (lift_div rejected size<2). The Div IR op + interp divide() + cranelift div helper already support size=1, so only the lift needed wiring: read AX (hi=AH=(AX>>8)&0xff, lo=AL=AX&0xff snapshotted before writes), Div{size:1}, quot->AL (WriteReg size 1), rem->AH (WriteTarget::HighByte). Impl: x86jit-core/src/lift/integer.rs lift_div 8-bit branch. Tests: differential.rs div8_idiv8_match_unicorn (unsigned + signed via dil/REX); jit.rs div8_idiv8_match_interp, div8_by_zero_raises_de, div8_overflow_raises_de. Regenerated backlog/docs/compat/{coverage.json,isa-coverage.md} (+2 lifted: Div_rm8, Idiv_rm8). cargo nextest run --features unicorn (minus fuzz): 511 passed. clippy/fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
