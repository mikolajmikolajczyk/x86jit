---
id: TASK-282
title: >-
  perf: ~30 host cycles per guest instruction — the lift, not the mid-end, is
  the ceiling
status: In Progress
assignee: []
created_date: '2026-07-22 11:42'
updated_date: '2026-07-22 13:42'
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
REDIRECTED 2026-07-22 after task-283's negative result. The next instrument is NOT another guest-side counter.

The embedder removed ~38 million helper calls per second (388M -> ~65k in a 10 s window, a 5000x drop) and fps, guest_exec, instructions per frame and MIPS were all unchanged within noise. Removing that much work with no effect is not evidence that the work was cheap — it is the signature of a core that is STALLED and has spare issue slots. 34 host cycles per guest instruction alongside indifference to work removal means the constraint is not instruction count.

That is now three consecutive attributions from this direction that were wrong while the underlying counts were right: task-220 read a per-instruction cost and inferred the lift; task-227 read 388M calls and inferred the barrier; this task's own description infers the lift again from 30 cycles/instruction. Each counter answers 'how much of something happens'. None answers 'what is the core waiting on', and that cannot be derived from them — which is why more of the same will keep producing true numbers with false conclusions.

NEXT STEP — host hardware counters, on the embedder's machine, ~30 seconds:

    perf stat -p <pid> -e cycles,instructions,cache-misses,LLC-load-misses,\
      branch-misses,stalled-cycles-frontend,stalled-cycles-backend -- sleep 10

Host IPC well below 1 with high stalled-cycles-backend says memory-bound, and the search moves to memory traffic (guest state materialization, working-set behaviour) rather than instruction count. High branch-misses says something else entirely. Either way it distinguishes hypotheses that no guest-side counter can.

Then X86JIT_PERF_MAP=1 plus perf record attributes the stalls to individual compiled blocks (symbols appear as jit_0x<guest_rip>), which is AC#1/#2 answered with measurement instead of inference.

AC#3 explicitly should NOT be attempted before that: choosing between lazy flags, guest-state materialization and memory-access lowering on present evidence would be a fifth guess, and the previous four (opt_level, IBTC, chaining, the watch barrier) all missed.

RESOLVED BY THE EMBEDDER'S perf stat 2026-07-22 — FRONTEND-bound, not memory-bound.

    cycles                  39,698,982,521
    instructions            40,488,120,386   -> IPC 1.02
    stalled-cycles-frontend 20,271,859,106   -> 51% of cycles
    iTLB-load-misses            38,094,571   -> 0.94 per kinstr
    L1-icache-load-misses       65,990,115
    L1-dcache-load-misses      326,580,286   -> 8 per kinstr, normal

Half the cycles are frontend stalls; data is fine. Host runs 4.05 G instructions/s against a guest ~100 M/s, so ~40 host instructions per guest instruction across the thread and ~30 inside guest_exec — which is the task title's '30 cycles', now known to be emitted instructions at IPC ~1 rather than stall cycles. The profile is flat: 58,599 blocks in the perf map, hottest 0.37% (jit_region_0x1b1b8da), ~83% of cycles in JIT'd code. Mono full-AOT has a huge code footprint, this engine expands it ~30x, and the result fits neither the op cache, nor L1i, nor the iTLB.

So there is nothing to optimise pointwise — no hot loop, no bad path. The only lever is average emitted-code density, which acts on all 58k blocks at once. AC#1/#2 answered below.

MEASURED HERE (density_tests, commit 114eee9), host instructions emitted, split into the fixed per-block cost and the marginal cost per additional guest instruction:

    shape            marginal   fixed/block
    alu reg,reg           3.0          49.0
    load                  2.0          27.0
    store                20.1          39.9
    sse scalar mul       15.0          11.0
    sse packed mul       14.0          11.0

This reproduces the embedder's 30x: a 2.9-instruction block with a store, a load and an ALU op is roughly 40 + 20 + 2 + 3 = 65 host instructions over 2.9 guest, i.e. ~22, and a block containing an SSE op lands near 30.

AC#3 — the largest components, with the split that decides what to do:
1. FIXED per-block cost, 11-49 host instructions, divided by only 2.9 guest instructions. At that block length this is roughly half the total. The lever is block LENGTH, not density: superblock formation. The embedder measured only 5-8% from regions with a tuned T2, which given this arithmetic means region formation is largely not succeeding on that code — and executed/chained (task-281) now measures exactly that.
2. STORE lowering at 20.1 marginal, against 1.0 with the watch gate removed entirely. Nearly all of it is the watch gate: a bounds-checked store itself is ~1 instruction. See TASK-283 — an inline bit test made this 41.3 and was reverted; a cheaper encoding (byte-per-page rather than bitmap, ~4 instructions) is the open idea.
3. SSE at 14-15 marginal, likely vector state materialisation, unexamined.

NOTE THAT THIS DIRECTION TRANSFERS, unlike the four failed attempts. Emitted-code density is a STATIC property: a percentage removed applies to every block regardless of what the core is doing. The earlier attempts measured an operation's latency in isolation, which does not compose into a stalled workload.

SEPARATE, CHEAPER LEAD (embedder's, not codegen): huge pages for the JIT code arena. 3.8M iTLB misses per second over code spread across tens of MiB; 2 MiB pages would remove most of them without touching lift quality. cranelift-jit allocates via MmapMut::map_anon, and x86jit already knows every function's address and length (codemap/perfmap registration), so MADV_HUGEPAGE is reachable. Unmeasured.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
