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

ANSWERED 2026-07-22 — the measurement above was run. **Frontend-bound.**

Embedder: unemups4 (PS4 emulator), Celeste retail, x86jit 8a67575, Ryzen 7 7840HS (Zen 4).
`perf stat -t <guest thread tid>` over 10 s of steady gameplay. `perf_event_paranoid=2`, so every
count is user-space only — which is exactly the domain of interest. Measured on the guest thread
alone, not the whole process, so the display and audio threads do not dilute it.

```
cycles                    39,698,982,521
instructions              40,488,120,386     IPC 1.02
stalled-cycles-frontend   20,271,859,106     51% of all cycles
iTLB-load-misses              38,094,571     0.94 per kinstr  (3.8M/s)
L1-icache-load-misses         65,990,115
L1-dcache-load-misses        326,580,286     8 per kinstr — unremarkable
branch-misses                112,643,529     2.7 per kinstr
cache-misses                 823,468,996
```

`stalled-cycles-backend` and `LLC-load-misses` read `<not supported>` on Zen 4 under these
generic names, so the backend half is not directly measured here — but the frontend half alone
accounts for 51% of cycles, and the data-side counters are unremarkable (8 L1d misses per
kinstr). The hypothesis this task redirected toward — memory-bound on data — is NOT what the
counters show.

**The expansion factor is real, and it is ~30x.** The host retires 4.05 G instructions/s on this
thread; the guest retires ~100 M/s (2.5M guest instructions per frame at 40 fps, from the
embedder's icount). That is 40 host instructions per guest instruction across the whole thread,
and ~30 counting only the 75% of thread time that is guest execution rather than the embedder's
HLE. So this task's original figure was right and its framing was too: at IPC 1.02, "30 cycles
per guest instruction" and "30 host instructions per guest instruction" are the same statement.
The lift IS the ceiling. What the counters add is WHY that ceiling bites — not because the
instructions are slow, but because there are so many of them that the frontend cannot deliver
them.

**The profile is flat.** `X86JIT_PERF_MAP=1` + `perf record` on the guest thread: 58,599 entries
in the map, hottest symbol 0.37%.

```
0.37%  jit_region_0x1b1b8da
0.28%  jit_0x1b60fec
0.25%  jit_0x1b61059
0.20%  jit_0x1b60fc0
0.20%  jit_0x1b6109e
```

By DSO: ~83% of guest-thread cycles in JIT-generated code, 17% in the embedder's Rust (largest
single native symbol: its PM4 command-buffer walk at 6.9%). AC#1/#2 therefore answer "nowhere in
particular" — which is itself the finding. This title is Mono full-AOT: an enormous code
footprint, expanded ~30x, touching tens of thousands of blocks per frame. It fits in no level of
the frontend — not the op cache, not L1i, not the iTLB.

WHAT THIS IMPLIES FOR AC#3. A flat profile means per-block or per-pattern work cannot pay: there
is no hot block to fix. The only lever that scales is AVERAGE emitted-code density, because every
percent applies to all 58k blocks at once. That reorders the candidate list — it favours whatever
shrinks the common-case instruction sequence (lazy flags, guest-state materialization) over
anything that speeds up a specific construct.

SECOND, INDEPENDENT LEVER, much cheaper than lift work: **huge pages for the JIT code arena.**
3.8M iTLB misses per second over a code arena spread across tens of megabytes is a large, purely
mechanical cost that does not require touching codegen quality at all. Worth doing first if only
because it is separable — it can be measured on its own, and it does not compete with AC#3.

CAVEAT on reading these numbers: this is ONE title on ONE host. Celeste's Mono AOT footprint is
close to a worst case for frontend pressure; a title with a tight native hot loop would likely
show the opposite balance. Before optimizing for this shape, it is worth checking whether
x86jit's own sqlite/lua workloads are frontend-stalled too — if they are not, the fix is being
designed against a single embedder's workload.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
