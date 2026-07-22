---
id: TASK-282
title: >-
  perf: ~30 host cycles per guest instruction — the lift, not the mid-end, is
  the ceiling
status: In Progress
assignee: []
created_date: '2026-07-22 11:42'
updated_date: '2026-07-22 15:12'
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
MEASURED 2026-07-22 — AC#1/#2/#3 answered. The attribution is FLAG MATERIALIZATION AT BLOCK EXIT.

Instrument: density_tests::host_instructions_per_guest_instruction and ::dump_one_shape
(x86jit-cranelift/src/codegen/mod.rs, both #[ignore], run explicitly). dump_one_shape honours
CHAIN=1 to terminate the shape with a real two-way chained exit instead of `hlt`.

    shape               total@16    hot@16  marg total    marg hot    cold %  chain fixed
    alu reg,reg               95        95         3.0         3.0        0%         56.0
    load                      57        49         2.0         2.0       14%         26.0
    store                    354       159        19.8         8.3       55%         36.7
    sse scalar mul           249       249        15.0        15.0        0%         18.0
    sse packed mul           233       233        14.0        14.0        0%         18.0

READING. The marginal cost of an extra guest instruction is SMALL (3.0 for ALU): the mid-end
already eliminates flags that are overwritten later in the same block, so intra-block density is
not the problem. The cost is FIXED PER BLOCK — 56 host instructions for a chained ALU block. At
the embedder's 2.9 guest instructions per block that is 56 + 2*3 = ~62 host instructions for ~3
guest ones, i.e. ~21x expansion of which almost all is the fixed term.

WHAT THE FIXED TERM IS. Disassembly of `add eax,ebx ; add eax,ebx ; jnz +2 ; hlt`:

    CF 4   PF 7   AF 7   ZF 5   SF 4   OF 8   = 35 of the ~62 host instructions

The last flag-setting instruction's flags are live across the block boundary, so exactly one full
six-flag materialization survives per block no matter how long the block is. This is why neither
dead-flag elimination (TASK-104) nor superblock formation removes it, and it is consistent with
regions having delivered only 5-8%.

Three secondary findings from the same dump:
  - ZF/SF/OF mask to the operand size with `andq` against a CONSTANT-POOL entry (`const(0)`,
    `const(1)`) instead of using the 32-bit subregister. Mechanical waste. -> TASK-284
  - AF and PF are 14 of the 35 and have no hot reader at all: grep of `offsets.af|offsets.pf`
    finds only assemble_rflags (pushfq/lahf/syscall) and eval_cond(Cond::Parity). x86 has no
    conditional branch on AF. -> TASK-285
  - every block emits `pushq %rbp / movq %rsp,%rbp` and tears it down on each of its 4 exits.
    Worth checking whether unwind_info is required in production codegen (perf-map unwinding
    depends on it).

FOLLOW-UPS, in order, each gated on the previous:
  TASK-284  narrow with ireduce instead of a pooled mask                     (mechanical, hours)
  TASK-285  defer AF/PF to stored sources, 14 -> 4 instructions              (~16% of a block)
  TASK-286  full lazy flags cc_op/cc_src/cc_dst, 35 -> ~6                    (~45%, plan first)

284+285 together are the CHEAP FALSIFIABLE PROBE for the direction: ~20% less hot code for a few
days of local reversible work. If the embedder measures no fps change from a 20% cut, the
'frontend-bound = hot code size' model is wrong and TASK-286 must not be started.

Still unexamined: SSE at 14-15 host instructions per op with 0% cold (MonoGame vector math), and
huge pages for the JIT code arena against the 0.94 iTLB misses/kinstr.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
