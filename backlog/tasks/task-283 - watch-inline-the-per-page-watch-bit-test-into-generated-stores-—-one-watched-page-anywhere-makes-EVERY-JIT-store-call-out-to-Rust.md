---
id: TASK-283
title: >-
  watch: inline the per-page watch-bit test into generated stores — one watched
  page anywhere makes EVERY JIT store call out to Rust
status: Done
assignee: []
created_date: '2026-07-22 12:53'
updated_date: '2026-07-22 13:12'
labels:
  - perf
  - memory
  - cranelift
dependencies: []
priority: high
ordinal: 313000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The Cranelift store gate for watched-range dirty tracking is keyed on a PROCESS-WIDE count, not on the address being stored to. From x86jit-cranelift/src/codegen/mod.rs:2446 (`note_watched_store`):

    let wc = load(watch_count_ptr);        // Memory::watch_count — process-wide
    let watched = icmp_ne(wc, 0);
    brif watched -> call note_watched_write_helper(mem_self, guest_addr, len)

The address is only examined AFTER the call, inside Memory::note_watched_write (x86jit-core/src/memory.rs:647), which walks the store's pages and tests `self.watch.is_watched(page)` — for the overwhelming majority of stores that test fails and the helper returns having done nothing.

Consequence for an embedder: watching ONE page anywhere in the process makes EVERY store out of compiled code a call into Rust, for the whole process, for as long as that page stays watched. The gate is all-or-nothing, so an embedder cannot reduce the traffic by watching less — only by watching nothing.

MEASURED IN unemups4 (PS4 emulator, Celeste retail, x86jit e776a90), using the helper counters from task-283:

  watching textures + index buffers + shader code ranges:
    helper calls: 392308578 per 10 s window
      note_watched_write_helper=388369775   <- 99.2% of all helper traffic
      div_helper=1167023  vec_maskmov_mem_helper=1462739  bmi_helper=1058935  (rest: thousands)

  same build, same title, dirty tracking switched to a source that never calls watch_range
  (so watch_count stays 0):
    helper calls: 2285216 per 10 s window
      note_watched_write_helper ABSENT from the table entirely
      div_helper=986327  vec_maskmov_mem_helper=1062655  bmi_helper=135124

~38 million calls per second, all of which exist to discover that the page is not watched. For calibration the same counter reports 0.00 per kinstr on x86jit's synthetics, 3.35 on sqlite, 2.55 on lua — this traffic is not inherent to the engine, it appears the moment an embedder watches anything.

The embedder tried the policy fix first (stop watching the ranges whose cache entries can never hit — its vertex ring and constant buffers, measured at zero clean cache hits). Helper calls went 388M -> 366M, i.e. unchanged, exactly as the global gate predicts. That confirmed the gate rather than the policy is the lever, which is why this is filed here.

HONEST NOTE ON THE PRIZE: the embedder has NOT established what this costs in wall time. Its guest_exec per frame measured the same (22-23.7 ms) with the barrier on and with watch_count at 0 — though that comparison is confounded (the zero-watch configuration re-uploads every GPU resource every submit, so its frames are 150 ms long and dominated by upload). A boot-phase throughput window measured 4425 MIPS with watching vs 7007 MIPS without, but that phase is spin-heavy and should not be read as a general number. So: the call count is certain and large, the per-call cost is evidently small (well-predicted branch, hot L1, immediate return), and the product of the two is unmeasured. A cheap microbenchmark on this side — a store-heavy loop with one unrelated watched page vs none — would price it properly before anyone spends much on the fix.

PROPOSED FIX: inline what the helper already does for the common case. The per-page bit test is `watch.is_watched(page)` over a bitmap whose base is stable for the run, so generated code can do: page = guest_addr >> CODE_PAGE_BITS; load the bitmap word; test the bit; call the helper only if set (and, as today, only if watch_count != 0). An unwatched page then costs a shift, a load and a test instead of a call, and the call survives only where it does real work.

Two details the implementation has to get right:
- A store can span two pages. Either test both pages inline, or test the first page and fall back to the helper whenever the store crosses a page boundary (`(addr & (PAGE-1)) + len > PAGE`), which is rare and keeps the inline path to a single test.
- The bitmap pointer must be reachable the same way watch_count_ptr is (through MemCtx), and it must stay live-loaded rather than baked, for the same task-217 reason: a watch installed by another thread mid-run has to become visible on the next store.

Keeping the existing watch_count gate in front of the bit test costs nothing and preserves the task-204 zero-cost-when-unwatched property for embedders that watch nothing at all.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 generated stores test the per-page watch bit inline and call note_watched_write_helper only when it is set
- [x] #2 a store crossing a page boundary is still recorded correctly (either both pages tested inline, or a helper fallback on the crossing case), covered by a test
- [x] #3 a watch installed by another thread mid-run is still seen by a running vCPU's next store (task-217 property preserved), covered by a test
- [x] #4 watch_dirty differential tests stay green: every store path the interpreter records, the JIT still records
- [x] #5 microbenchmark reports helper calls and wall time for a store-heavy loop with (a) nothing watched, (b) one unrelated page watched, (c) the stored-to page watched — before and after
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented. The store gate now tests the store's own page inline; the helper call survives only where it does work.

SHAPE. note_watched_store (codegen/mod.rs) keeps the process-wide watch_count gate in front — an embedder watching nothing still pays only a load and a branch (task-204) — and behind it adds: page = addr >> 12; load the bitmap word at (page >> 6); test bit (page & 63); call the helper only if set, OR if the store crosses a page boundary. MemCtx gains watch_bits_ptr, read LIVE like watch_count_ptr so a watch installed mid-run by another thread is seen by the next store (task-217).

PAGE CROSSING — helper fallback, not a second inline test. When (addr & 0xFFF) + size > 4096 the helper runs and walks every page the store touches. Crossing is rare and the inline path stays one test. The guard was written BEFORE the implementation, confirmed to pass against the old code (so it is not trivially green), then confirmed to FAIL against the naive first-page-only version (left: [] vs right: [(0x5000, 4096)]). This facility has already shipped two silent under-reporting bugs (task-273, task-275) that reached the embedder as visible corruption, which is why the crossing case has a test rather than a comment.

THE OUT-OF-BOUNDS RISK IS CLOSED. The inlined load has no bounds check; it is sound only because inlined stores are bounds-checked against MemCtx.size first and WatchPages is sized over guest_base + backing len (from_host_ram asserts size - guest_base <= backing len). That is three facts in two crates. Memory::watch_bits_cover_size() now states the invariant and MemCtx::for_memory debug_asserts it at run start, so a future change to any of the three fails loudly instead of emitting an out-of-bounds load — host UB rather than a panic.

MEASURED, AC#5, x86jit-cranelift/tests/watch_gate_cost.rs (ignored; run with --release -- --ignored --nocapture). 20M-iteration store loop, best of 3, ns per store and helper calls:

                                  BEFORE                    AFTER
  nothing watched            4.270   0 calls          4.198   0 calls
  one UNRELATED page         6.538  20M calls         4.875   0 calls
  the stored-to page         6.850  20M calls         7.151  20M calls

So: the unrelated-page case loses its calls entirely and drops from +53.1% to +16.1% over unwatched; the nothing-watched path is unchanged (task-204 preserved); the genuinely-watched path costs 4.4% more because it now does the inline test AND the call, which is the honest price of the path that does real work.

For the reporting embedder (388M note_watched_write calls per 10 s window, ~1.55M per frame): ~1.66 ns saved per call is roughly 2.6 ms of a 22 ms guest_exec, about 12%. Their watched pages are a small fraction of their stores, so nearly all of those calls should disappear.

NOT CHANGED, deliberately: the task-217 publication race. WatchPages::watch does fetch_or(bit, Relaxed) then count.fetch_add(Relaxed), while the reader checks count then the bit, so a reader can see count != 0 with a stale bit word. That race exists TODAY inside the helper — inlining does not introduce it, it only narrows the window by removing the call's latency. The consequence is unchanged: a few stores at the instant of installation may be missed, later ones are seen. Closing it properly needs Acquire on the store path (LDAR on ARM, on the hot path), which is a separate decision and should not be made silently here.

VERIFICATION. cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' -> 900/900; clippy -D warnings clean; fmt clean; aarch64 cross-target check clean. The watch_dirty differential suite (AC#4) is green, including the task-273 vector/call store coverage and the task-275 above-4-GiB case.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
