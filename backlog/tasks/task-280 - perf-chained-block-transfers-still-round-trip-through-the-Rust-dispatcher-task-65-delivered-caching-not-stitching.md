---
id: TASK-280
title: >-
  perf: chained block transfers still round-trip through the Rust dispatcher
  (task-65 delivered caching, not stitching)
status: To Do
assignee: []
created_date: '2026-07-22 09:14'
updated_date: '2026-07-22 09:32'
labels:
  - perf
  - jit
  - dispatch
  - cranelift
dependencies: []
priority: low
ordinal: 310000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`chain_or_link` (x86jit-cranelift/src/codegen/mod.rs:3436) emits, for a filled link slot:

    self.store_mem(MEMCTX_NEXT_ENTRY, entry);
    self.ret(RET_CHAIN);

so compiled code RETURNS to Rust. The dispatcher (x86jit-core/src/vm.rs, RET_CHAIN arm) then does `cur = next_entry` and re-enters via `call_block`. Every block-to-block transfer is therefore a full function return + Rust loop iteration + indirect call + prologue.

What the link slot bought is skipping `resolve` (the RwLock + hash lookup). That is real and valuable. But it is NOT what §12 M5 means by block chaining — spec.md:940 defines it as 'blocks jump straight into each other *without returning to the dispatcher*', and spec.md:1002 lists it that way. TASK-65 carries exactly that title and is marked Done with 'Delivered pre-migration', no detail. The milestone therefore reads as complete while the dispatcher round-trip it was meant to remove is still there. Filing this so the headroom is not invisible; no blame, the imported task simply lost its definition.

SIZE OF THE PRIZE — measured, and smaller than it first looks. From the bench counters: hotloop does 10,999,993 chained transfers in ~39 ms and fib32 21,147,457 in ~100 ms, i.e. 3.5-4.7 ns per transfer INCLUDING the guest work of the block. So the round-trip itself is at most ~3.5 ns and probably 2-3 ns — the indirect call target repeats and the return address predicts, so an out-of-order core hides most of it. It is NOT the 10-25 ns a naive call/return estimate suggests; that estimate was checked and discarded.

Applied to the reporting embedder (unemups4, Celeste): ~1,000,000 chained transfers per frame against 24 ms of guest_exec gives roughly 2-3.5 ms/frame, about 8-15% of guest execution. Worth having, not transformative. Any plan should be sized against that, not against a hoped-for multiple.

THE HAZARD IS ALREADY WRITTEN DOWN. spec.md:940 and :1087: with true chaining `blocks_run` stops ticking, so a chained loop never yields `BudgetExhausted` and starves other vcpus. The spec's instruction is 'Decide it with chaining, not after' — a periodic counter check compiled into chained edges, or an exit flag polled at back-edges. Region codegen already carries a per-block fuel gate (codegen/mod.rs translate_region), which is the shape to reuse. SMC invalidation is the other half: a directly-stitched jump has to be un-stitched when `invalidate_links` fires, where today zeroing the slot is enough because the dispatcher re-reads it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A chained edge with a resolved target transfers control without returning to the Rust dispatcher, verified by counting dispatcher re-entries (not just by timing)
- [ ] #2 Preemption survives: a tight chained guest loop still yields BudgetExhausted, and a multi-vcpu run does not starve — with a test that fails against a naive always-stitch implementation
- [ ] #3 SMC invalidation un-stitches a directly-chained edge; the existing invalidate_links tests still pass and one covers a write landing on a chained-into block
- [ ] #4 Measured on the bench AND reported to the embedder; the change is kept only if it moves guest_exec on a real workload, given a predicted ceiling of roughly 8-15%
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
MEASURED BEFORE STARTING — DO NOT BUILD THIS AS SPECIFIED. The description's '8-15%' estimate is WRONG and is superseded by this note.

That estimate divided a block's total time by the number of transfers, which mixes the guest work inside the block with the dispatch overhead around it. Measuring the overhead directly instead (microbenchmark modelling the vm.rs inner loop: extern "C" indirect call, the quantum/ctx.fuel/blocks_run accounting, the match, cur = next_entry; against the same block body with no call boundary at all):

    round-trip (today):   1.243 ns/transfer
    stitched  (ideal):    0.416 ns/transfer
    dispatch overhead:    0.827 ns/transfer

0.827 ns is generous toward stitching: the 'ideal' side carries NO fuel check, while real stitching must keep one on chained edges (spec.md:940/:1087 preemption requirement), so part of that would come straight back.

Applied to the reporting workload (unemups4/Celeste, ~1,000,000 chained transfers per frame, 24 ms guest_exec): 0.83 ms/frame, about 3.4%. That is the CEILING.

The cost side is unchanged and large: Cranelift tail calls require CallConv::Tail on every block, which is not C-compatible, so the Rust dispatcher can no longer call blocks directly — it needs wasmtime-style extern "C" trampolines. Plus moving preemption into compiled code and un-stitching directly-jumped edges on SMC invalidation. A major ABI change and two new hazard classes for ~3%.

RECOMMENDATION: leave this To Do at Low. Revisit only if a workload appears whose profile is dominated by chained transfers over very small blocks — i.e. where the 0.83 ns is a large share of per-block time. Celeste is not that workload.

WHAT THIS RULES IN. Indirect branches are ~0.5% of Celeste's control transfers (TASK-278's negative result) and the dispatcher round-trip is ~3.4% of its guest_exec. Together that means dispatch is NOT where its 24 ms goes — the time is inside the compiled code itself. The next step is a sampling profile in compiled code (X86JIT_PERF_MAP=1 plus perf, on the embedder's side where the real workload runs), not further dispatch micro-optimization. Two dispatch-side optimizations have now failed to move that workload; a third should not be started without a profile pointing at it.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
