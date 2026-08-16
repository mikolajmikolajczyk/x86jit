---
id: TASK-347
title: >-
  jit: the two tiers report different fault addresses for an access crossing the
  top of the span
status: To Do
assignee: []
created_date: '2026-08-15 13:27'
labels:
  - code-review
dependencies: []
priority: high
ordinal: 383000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`checked_addr`'s fault block (codegen/mod.rs:2464-2493) names the FIRST UNBACKED BYTE (`memsize`). The interpreter names the BASE of the failing sub-access -- the operand base for a scalar access, an 8-byte-granular sub-part for a vector one. They coincide only when the span top happens to land on the interpreter's grid.

Probe, flat span 0x10000, whole span mapped:

    mov rax,[rbx]     base=0xfffc   interp 0xfffc   jit 0x10000   DIVERGE
    mov [rbx],rax     base=0xfffc   interp 0xfffc   jit 0x10000   DIVERGE
    movups xmm0,[rbx] base=0xfffc   interp 0xfffc   jit 0x10000   DIVERGE
    vmovups ymm0,[rbx] base=0xfff4  interp 0xfffc   jit 0x10000   DIVERGE
    mov eax,[rbx] fully past end    interp 0x10100  jit 0x10100   AGREE

The comment at mod.rs:2468-2470 asserts the opposite: "The interpreter splits a 256-bit access into 16-byte loads and already names the failing half; this makes the JIT agree." It splits at EIGHT bytes (interp/mod.rs:5134-5149, vpart(..., 8)), and the address it names is still a MAPPED byte -- exactly the failure that comment argues against ("the embedder would map it again, retry, fault identically, and loop"). The JIT was fixed and the interpreter was not.

THE EXISTING TEST HIDES IT. jit.rs:5153 extract_lane_mem_dst_straddle_fault_match_interp asserts the two tiers report the same fault, and picks TOP = DST + 16 -- a multiple of the interpreter's 8-byte grid. Same test shape with TOP moved off the grid:

    TOP=0x8010: interp 0x8010  jit 0x8010  SAME
    TOP=0x8014: interp 0x8010  jit 0x8014  DIVERGE
    TOP=0x8018: interp 0x8010  jit 0x8018  DIVERGE
    TOP=0x801c: interp 0x8010  jit 0x801c  DIVERGE

Three of four offsets diverge; the test uses the one that agrees.

Guest sees a tier-dependent Exit payload: a demand-paging embedder is handed a different page depending on whether the instruction ran interpreted (cold, MMIO-deferred, tier-up warm-up) or compiled -- and on the interpreter leg the address is already mapped, so map-and-retry loops.

Blast radius: one site (checked_addr) reached by every inlined access -- 8 store sites and ~60 load sites in vector.rs; on the interpreter side vpart/region_at for every access.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Both tiers name the same address for an access that crosses the top of the span, whatever the alignment of the top
- [ ] #2 The reported address is never one that is already mapped, on either tier
- [ ] #3 The straddle test sweeps the top across all offsets in a sub-access rather than sitting on the interpreter's grid
- [ ] #4 The comment at codegen/mod.rs:2468-2470 states the real split width and the real agreement
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
