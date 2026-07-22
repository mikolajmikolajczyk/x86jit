---
id: TASK-282
title: >-
  perf: ~30 host cycles per guest instruction — the lift, not the mid-end, is
  the ceiling
status: In Progress
assignee: []
created_date: '2026-07-22 11:42'
updated_date: '2026-07-22 13:29'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
STEP 1 DONE — instruments built, hypothesis NOT yet tested (that measurement has to run on the embedder's workload).

FIRST FINDING, WHICH QUALIFIES THE TASK'S PREMISE. The description infers 'the cost is per instruction, so it is the lift'. Measured locally with task-281's counter wired into the bench (commit d706673), guest MIPS and the implied cost per guest instruction at ~4.5 GHz, alongside average compiled-unit length (executed / chained):

    sha256    44.4 instr/block   2429 MIPS    1.9 cycles/instr
    memcpy     8.0               1083         4.2
    hotloop    2.5                688         6.5
    simd       4.0                683         6.6
    fib32      2.7                525         8.6
    indirect   5.3                347        13.0
    CELESTE    2.9                130        34.6

So the lift does NOT have a ~30-cycle floor — it reaches 1.9. The measurement in the description is sound; the inference from it is not.

Cost per instruction falls sharply with block length, and Celeste sits in the short-block regime (2.9 instructions per compiled unit, from its own executed/chained). But hotloop at 2.5 instr/block runs 5x faster than Celeste, so block length is necessary and not sufficient. That unexplained 5x is what to chase; my benches cannot reproduce it (tiny working sets, everything L1-resident, perfectly predicted branches).

INSTRUMENT FOR THE LEADING CANDIDATE (commit e776a90). Helper calls: a C-ABI exit running a whole interpreter op, tens to hundreds of cycles each, so even a low rate is a large share of time. Counted in call_helper, per helper, always on (the counter is noise beside the call it sits next to). Read via Backend::helper_calls() — on the trait, because a Vm owns its backend boxed. Bench reports calls per 1000 guest instructions.

Local reading: synthetic workloads 0.00/kinstr; sqlite 3.35 and lua 2.55, all string_helper (rep movs/stos, legitimately bulk). Helper traffic is therefore not inherent to the engine — which is what makes a high reading on Celeste meaningful rather than expected.

NEXT, in order:
1. Embedder runs vm.backend.helper_calls() over a gameplay window. ~0/kinstr exonerates helpers; tens/kinstr names the helper to lower natively (task-236 already ranks them).
2. If helpers are exonerated: AC#1 locally — disassemble representative hot blocks and count host instructions per guest instruction. Static, so immune to the cache/timing differences that make my benches unrepresentative. The srcloc table (host_off -> guest_rip) is already collected in compile_with, so the attribution is mostly a matter of reading it.
3. Only then choose between lazy flags / state materialization / memory lowering. Picking one before step 2 would be a fourth guess this session; the previous three (opt_level, IBTC, chaining) all missed.

REDIRECTED 2026-07-22 after task-283's negative result. The next instrument is NOT another guest-side counter.

The embedder removed ~38 million helper calls per second (388M -> ~65k in a 10 s window, a 5000x drop) and fps, guest_exec, instructions per frame and MIPS were all unchanged within noise. Removing that much work with no effect is not evidence that the work was cheap — it is the signature of a core that is STALLED and has spare issue slots. 34 host cycles per guest instruction alongside indifference to work removal means the constraint is not instruction count.

That is now three consecutive attributions from this direction that were wrong while the underlying counts were right: task-220 read a per-instruction cost and inferred the lift; task-227 read 388M calls and inferred the barrier; this task's own description infers the lift again from 30 cycles/instruction. Each counter answers 'how much of something happens'. None answers 'what is the core waiting on', and that cannot be derived from them — which is why more of the same will keep producing true numbers with false conclusions.

NEXT STEP — host hardware counters, on the embedder's machine, ~30 seconds:

    perf stat -p <pid> -e cycles,instructions,cache-misses,LLC-load-misses,\
      branch-misses,stalled-cycles-frontend,stalled-cycles-backend -- sleep 10

Host IPC well below 1 with high stalled-cycles-backend says memory-bound, and the search moves to memory traffic (guest state materialization, working-set behaviour) rather than instruction count. High branch-misses says something else entirely. Either way it distinguishes hypotheses that no guest-side counter can.

Then X86JIT_PERF_MAP=1 plus perf record attributes the stalls to individual compiled blocks (symbols appear as jit_0x<guest_rip>), which is AC#1/#2 answered with measurement instead of inference.

AC#3 explicitly should NOT be attempted before that: choosing between lazy flags, guest-state materialization and memory-access lowering on present evidence would be a fifth guess, and the previous four (opt_level, IBTC, chaining, the watch barrier) all missed.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
