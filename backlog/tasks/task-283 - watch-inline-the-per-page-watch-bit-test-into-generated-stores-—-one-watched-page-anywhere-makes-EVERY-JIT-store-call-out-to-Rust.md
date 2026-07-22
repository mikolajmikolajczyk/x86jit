---
id: TASK-283
title: >-
  watch: inline the per-page watch-bit test into generated stores — one watched
  page anywhere makes EVERY JIT store call out to Rust
status: To Do
assignee: []
created_date: '2026-07-22 12:53'
updated_date: '2026-07-22 13:41'
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
NEGATIVE RESULT FROM THE EMBEDDER (2026-07-22). The change works mechanically and buys nothing measurable.

                              note_watched_write   helpers/kinstr   fps            guest_exec      MIPS
  baseline (watch everything)        388,369,775          342.07   36.2/35.6/39   21.6/22.2/19.8   135-150
  policy fix alone                   366,489,935          348.28   34.6/34.6      22.7/22.7        138-142
  policy fix + this inline check          49,686-74,023    2.72-2.98   35.4-37.7   21.0-22.6       130-144

A 5000x reduction in helper calls. Helper traffic fell to 2.72/kinstr, right where x86jit's own sqlite (3.35) and lua (2.55) sit. fps, guest_exec, instructions per frame and MIPS all unchanged within noise: ~38 million calls per second removed with no measurable effect.

WHERE MY PROJECTION WENT WRONG, precisely. The microbenchmark in this repo measured 2.27 ns per call and that number is real. The error was multiplying it by the call count and reporting '~2.6 ms of a 22 ms guest_exec, about 12%'. That arithmetic assumes an operation's cost adds to a workload's wall time. It does not when the core is stalled on something else. The benchmark loop is 'mov [rdi], eax; dec ecx; jnz L' — three instructions, one dependency chain, nothing to overlap with — so the call's latency is fully exposed. In the real title the core has spare issue slots and the call disappears into them.

GENERAL FORM, which explains every perf attempt in this session: a microbenchmark measures an operation's LATENCY IN ISOLATION. A real workload's speed is set by its binding constraint, and work that fits in the shadow of that constraint is free however often it runs. opt_level=Speed (better code), the IBTC probe (faster dispatch), and this (fewer calls) all reduced work that was not the constraint. task-280's dispatcher round-trip was rejected on the same grounds and that rejection now looks correct for the same reason, not merely because 3.4% was small.

KEEP THE CHANGE, on changed grounds. Both halves are complementary — the inline check makes an unwatched page cheap, the embedder's policy makes the ring pages unwatched, and neither alone produces the drop. It removes pathological behaviour (one watched page anywhere calling out on every store process-wide), it is free, and it could matter on a weaker core or a workload actually bound by stores. It is NOT a measured win and should not be cited as one.

WHAT THIS RULES OUT. Guest-side counters — retired, helper_calls, chained — all answer 'how much of something happens'. None answers 'what is the core waiting on', and that quantity cannot be derived from them. Three consecutive times one of these counters has pointed at a real, large number whose removal changed nothing (task-220 attributed cost to the lift, task-227 to the barrier, this one to helper traffic). The number was true each time; the attribution was not. See TASK-282 for the next instrument.

REVERTED 2026-07-22 — the change was a net NEGATIVE once the real constraint was known.

The embedder's perf stat (see TASK-282) showed Celeste is FRONTEND-bound: 51% of cycles stalled on the frontend, IPC 1.02, iTLB misses 0.94 per thousand host instructions, flat profile over 58,599 blocks. So the binding constraint is emitted-code FOOTPRINT, not dynamic work.

Measured with the new density harness (commit 114eee9), host instructions emitted per guest store:

    no gate at all                                      1.0
    pre-task-283 (watch_count gate + helper call)      20.1
    post-task-283 (+ inline page-bit test)             41.3

The inline test DOUBLED the code emitted for every guest store. Stores are roughly half a block at the embedder's 2.9 guest instructions per block, so that is about +10 host instructions on a ~91-instruction block: ~11% more code footprint, in a workload limited by exactly that. The embedder's 'unchanged within noise' is consistent with a small harm hidden in noise or offset by the calls removed.

So this traded dynamic helper calls — which measured as FREE, because the stalled core has spare issue slots — for static code size, which is the one thing that is not free here. The same class of error as the earlier attempts, in the opposite direction: optimising a quantity that was not the constraint, and paying in the one that is.

Reverted the inline test and the MemCtx.watch_bits_ptr / Memory::watch_bits_ptr / watch_bits_cover_size scaffolding with it (dead without the test). KEPT: the page-straddle differential test (documents the requirement and guards any future attempt), and tests/watch_gate_cost.rs (the microbenchmark that priced the call). The embedder's own watch-policy change is unaffected and still worth having.

IF THIS IS REATTEMPTED, the constraint is instruction count, not call count. A byte-per-page table instead of a bitmap would make the test ~4 host instructions (shift, add, load byte, test) rather than ~15, with no bit extraction and no separate page-crossing test — at 1 byte per 4 KiB page instead of 1 bit (16 MiB of lazily-committed virtual per 64 GiB of arena). That would be worth measuring; the bitmap version is not.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
