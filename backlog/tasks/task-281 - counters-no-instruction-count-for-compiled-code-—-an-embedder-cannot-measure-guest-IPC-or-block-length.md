---
id: TASK-281
title: >-
  counters: no instruction count for compiled code — an embedder cannot measure
  guest IPC or block length
status: Done
assignee: []
created_date: '2026-07-22 09:51'
updated_date: '2026-07-22 11:22'
labels:
  - diag
  - perf
dependencies: []
priority: medium
ordinal: 311000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Vcpu::retired_instructions ticks only on the interpreter path. The field docs in x86jit-core/src/vm.rs:670-676 state it plainly: compiled Long64/Compat32 blocks do NOT tick it, because charging retirement inside compiled code would need codegen changes that are deliberately avoided, so on a 64-bit guest it counts only the occasional interpreter single-step (MMIO retry, pre-tier-up execution).

That is a defensible choice for the counter's stated purpose — a deterministic virtual-time base for a scheduler. But it leaves an embedder with no way to count the instructions that actually execute.

Concretely, from unemups4 (Celeste, retail PS4 title): guest execution is about 25 ms of a 40 ms gameplay frame, 99% of it on-core, so it is genuine computation. Wiring up retired_instructions produced about 23 thousand instructions in a 10 s window — the interpreter single-steps — while chained block transitions ran roughly 1 million per FRAME. The counter is off by orders of magnitude from what executed, so it cannot answer either question that decides where optimization effort goes:

- how far from native are we per instruction? Celeste holds 60 fps on a 1.6 GHz Jaguar core; we need 25 ms per frame on a far stronger CPU. Without an instruction count that gap stays an inference from wall clock, and the two candidate explanations (poor codegen vs an abnormal amount of guest work) call for completely different work.
- what is the average length of a compiled unit? retired divided by chained gives it directly, and that is the number that says whether superblock formation has anything to chew on and whether caps of max_blocks 16 / max_icount 256 are sized right.

Note this does NOT require per-instruction accounting in compiled code, which is what the current docs rule out. Each block's instruction count is already known at lift time, so a single add of that constant on block entry is one increment per BLOCK, not per instruction — the standard approach. A region knows its own total the same way.

Worth considering whether this belongs on the existing counter or on a separate one: retired_instructions is documented as a deterministic virtual-time base, and making it jump by a block's worth at a time changes its granularity for any scheduler relying on it. A separate executed-instruction counter, or an opt-in, may be the safer shape.

Filed from unemups4, where the measurement is blocked. See unemups4 task-220.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 an embedder can obtain a count of guest instructions executed in compiled blocks and regions, not only on the interpreter path
- [x] #2 the accounting costs at most one increment per block or region entry, not per instruction
- [x] #3 the deterministic virtual-time guarantee of the existing retired_instructions is either preserved or the change is on a separate counter
- [x] #4 measured cost of the added accounting on the bench suite is recorded
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented. Vcpu::executed_instructions() counts both tiers; opt-in on the JIT.

SHAPE. Compiled code adds IrBlock::icount (the lifter already records it) at block entry — one add per BLOCK, never per instruction, which is what the existing docs rule out. A region adds each guest block's count at the fuel gate it already passes through, so a multi-block unit stays exact. The interpreter feeds the same counter from info.retired, so the total covers whichever tier ran.

SEPARATE COUNTER, as the task suggested. retired_instructions is untouched and keeps its interpreter-only, per-instruction granularity — a scheduler reading it as a deterministic virtual-time base sees no change.

OPT-IN (JitBackend::enable_icount(), before the first compile). Measured cost when ENABLED, 2 alternating rounds of 5 iters: hotloop 41.1 -> 43.6 ms (+6.1%), fib32 100.6 -> 102.2 ms (+1.6%), memcpy within noise. That is too much to charge every embedder for a diagnostic, particularly having just rejected a 3.4% ceiling as insufficient reason to rebuild the block ABI in TASK-280. With it off the cost is within noise. State is visible via Backend::codegen_description() (icount=true|false) so nobody reads zeros believing they are real; the bench honours X86JIT_ICOUNT=1.

TRAP HIT AND FIXED — worth knowing. The first version moved MemCtx from a local in run_inner to a &mut parameter, to flush the counter in one place instead of at the inner loop's 15 exits. That cost +5.4% on fib32 (98.4/98.2 -> 103.3/103.9, consistent across rounds) WITH THE FEATURE DISABLED: the compiler stops treating ctx as a local. Confirmed by reverting only that move (back to 98.8). Fixed by using the pointer pattern MemCtx already has for ret_stack/watch_count_ptr: MemCtx.icount_ptr points at the vcpu's counter, compiled code does the read-modify-write through it, ctx stays a local, no flush needed. The reason is recorded at the field so it is not 'simplified' back.

CORRECTNESS. x86jit-cranelift/tests/icount.rs: the interpreter is the oracle and the compiled count must match it exactly — verified over tier_up_after 0/1/16 and eager on a 5000-iteration loop (10001 instructions, exact). Also asserts retired lags executed (proving the compiled path was actually exercised), that executed/chained gives ~2 for a two-instruction loop block, and that the accounting is genuinely off by default.

VERIFICATION. cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' -> 896/896; clippy -D warnings clean; fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
