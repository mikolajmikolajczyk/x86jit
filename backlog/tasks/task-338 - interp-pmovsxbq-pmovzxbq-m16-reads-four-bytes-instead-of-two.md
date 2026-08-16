---
id: TASK-338
title: 'interp: pmovsxbq/pmovzxbq m16 reads four bytes instead of two'
status: To Do
assignee: []
created_date: '2026-08-15 13:24'
labels:
  - m8-simd
dependencies: []
priority: medium
ordinal: 374000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
SDM Vol 2B, PMOVSX: the operand is `PMOVSXBQ xmm1, xmm2/m16`.

`vload`'s fallthrough arm (interp/mod.rs:5148) hardcodes `vpart(..., 4)`, while `exec_v_p_mov_extend_m` (interp/vector.rs:375) computes nbytes = (16/to)*from, which is 2 for the bq forms.

    2-byte operand at the last 2 bytes of the mapped span:
    interp: UnmappedMemory { addr: 65534, access: Read }   xmm0 = 0
    jit   : Hlt   xmm0 = 0x000000000000007fffffffffffffff81
    MMIO form: MmioRead addr=0x4000 size=4     <- an m16 operand announced as 4 bytes

`emit_v_p_mov_extend_m` (codegen/vector.rs:1244) handles it explicitly (`_ => types::I16, // bq: 2 bytes`); the interpreter does not.

Guest sees a spurious #PF when the operand sits in the last two bytes of a page, a wrong-width MMIO transaction, and a jit != interp divergence.

Blast radius: the only vload caller that can pass a size other than 4/8/16 is exec_v_p_mov_extend_m; of the six extend shapes only from=1,to=8 yields 2.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 vload handles a 2-byte access; the m16 form neither over-reads nor announces the wrong MMIO width
- [ ] #2 A test pins the operand at the last two bytes of a mapping and requires no fault
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
