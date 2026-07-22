---
id: TASK-280
title: >-
  perf: chained block transfers still round-trip through the Rust dispatcher
  (task-65 delivered caching, not stitching)
status: To Do
assignee: []
created_date: '2026-07-22 09:14'
labels:
  - perf
  - jit
  - dispatch
  - cranelift
dependencies: []
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

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
