---
id: TASK-282
title: >-
  perf: ~30 host cycles per guest instruction — the lift, not the mid-end, is
  the ceiling
status: In Progress
assignee: []
created_date: '2026-07-22 11:42'
updated_date: '2026-07-22 11:51'
labels:
  - perf
  - lift
dependencies: []
priority: high
ordinal: 312000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measured from the unemups4 embedder (Celeste, retail PS4 title) now that task-281 gives an executed-instruction count. Three consecutive gameplay windows, all counters sampled at the same frame boundary:

    34.44 fps   guest_exec 20.184 ms/frame   2.92 M instr/frame   145 MIPS
    38.06 fps   guest_exec 20.308 ms/frame   2.63 M instr/frame   129 MIPS
    50.21 fps   guest_exec  9.180 ms/frame   1.23 M instr/frame   133 MIPS

Stable at 129-145 MIPS. That is roughly 7 ns per guest instruction, or about 30 host cycles each on a modern desktop CPU.

Two things follow, and the second is the point.

THE GUEST IS NOT DOING TOO MUCH WORK. 2.6-2.9 M instructions per frame at 60 fps is about 175 M instr/s, which a 1.6 GHz Jaguar handles without difficulty — and the real console does hold 60 fps on this title. The instruction count is ordinary. What is not ordinary is that we need 20 ms of a far stronger CPU to execute it.

THE COST IS PER INSTRUCTION, SO IT IS THE LIFT. Thirty cycles for an average x86 instruction is not a mid-end problem; a good JIT is 1-3 cycles for a simple one. It is how much host code the lifter emits per guest instruction: guest-state materialization at block boundaries, flag computation, memory-access lowering.

That reading is consistent with everything tuning has failed to achieve in that embedder. opt_level none -> Speed measured as no change at all on this title. The IBTC miss-path probe measured as no change (indirect dispatch is 0.5% of its control transfers). Superblocks with a tuned T2 gave 5-8%. Three separate mid-end and dispatch improvements totalling a few percent against a 6-12x gap is what a per-instruction cost floor looks like.

Worth measuring before choosing a direction, since the same embedder has repeatedly had its confident hypotheses refuted by data:
- how many host instructions does a representative lifted block emit per guest instruction? A disassembly of a few hot blocks against their guest source answers it directly.
- what fraction is flag handling? state.rs:123 records lazy flags (Variant B) as deliberately deferred, with compile-time dead-flag elimination as the substitute (lift/mod.rs:767). Dead-flag elimination only removes what it can prove is unread, and superblock formation just widened the window it can see — measuring how much flag work survives inside a region would say whether Variant B is now the payoff it was expected to be.
- how much is guest-state spill and reload at block boundaries, and how far does region formation already reduce it?

The embedder-side numbers are in unemups4 task-220 and its commits; the instruction counter itself is task-281 here.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 host instructions emitted per guest instruction is measured for representative hot blocks, not estimated
- [ ] #2 the cost is attributed across flag handling, guest-state materialization and memory-access lowering, with numbers
- [ ] #3 the largest attributed component has a concrete proposal with an expected gain, or is recorded as already near its floor
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
