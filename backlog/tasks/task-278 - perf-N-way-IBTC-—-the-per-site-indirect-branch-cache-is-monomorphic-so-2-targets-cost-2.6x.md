---
id: TASK-278
title: >-
  perf: N-way IBTC — the per-site indirect-branch cache is monomorphic, so 2+
  targets cost 2.6x
status: To Do
assignee: []
created_date: '2026-07-22 07:09'
labels:
  - perf
  - jit
  - cranelift
  - dispatch
dependencies: []
ordinal: 308000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`ibtc_or_miss` (x86jit-cranelift/src/codegen/mod.rs:3463) holds ONE `desc` per call site, and that desc holds a single `(cached_target, entry)` pair. Any target mismatch branches to `miss` -> `RET_IBTC_MISS` -> back to the dispatcher for a full resolve. So a call site that alternates between two or more targets misses essentially every time.

MEASURED (this host, x86_64, --release, 3 iters, at HEAD = a57447b). The `indirect` bench workload does 1,000,000 indirect calls; INDIRECT_M is the number of distinct leaf targets an LCG picks from. Sweeping it (INDIRECT_EXPECT recomputed per M):

    targets   run        ns/call
    1         13.31 ms   13.3
    2         35.11 ms   35.1
    4         42.58 ms   42.6
    8         48.57 ms   48.6
    16        50.90 ms   50.9

The shape is a CLIFF, not a slope: 1 -> 2 targets costs 2.6x, while 2 -> 16 adds only 45% on top. The cost is not the number of targets, it is that the cache holds exactly one entry. At M=16, 37.6 of the 50.9 ns/call — 74% of the workload — is miss overhead over the monomorphic case.

Proposal: make the per-site IBTC N-way set-associative (4 or 8 entries per site), so a site with <= N hot targets chains instead of returning to the dispatcher. The inline sequence stays a load + compare per way, or a small tag-indexed probe; the miss path is unchanged.

Relevance: this is the draw-call / virtual-dispatch shape (the workload's own doc comment says so). The unemups4 embedder runs Celeste, a MonoGame/C# title, where virtual dispatch is pervasive, and TASK-276 recorded that 10.9% of slow-frame samples sit in one 0x3c-byte loop. Guest throughput is that embedder's dominant cost.

UNVERIFIED ASSUMPTION, resolve before building: the claim that 4 ways is enough rests on the general expectation that a real vtable call site has 1-4 hot targets, NOT on any measurement of real guest code. The bench's uniform-random pick over 16 is a deliberate worst case. Measure the actual per-site target distribution on a real workload first — if sites are genuinely highly polymorphic, an N-way cache buys much less than the table above suggests, and the answer may instead be a different miss path (e.g. a cheaper dispatcher return).

Reproduce the sweep: edit INDIRECT_M in x86jit-bench/src/workloads.rs (and INDIRECT_EXPECT to match — the values are 1:000f4240, 2:0044ac50, 4:00af7fac, 8:01852bc0, 16:03307070), then `cargo run -p x86jit-bench --release -- record --iters 3`.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The per-site target distribution is measured on a real guest workload (a game or a large binary), and the chosen associativity is justified by that measurement rather than by the synthetic bench
- [ ] #2 The IBTC serves multiple targets per call site; the indirect bench at M=4 lands materially closer to the M=1 monomorphic number than to today's M=4 figure
- [ ] #3 No regression on the monomorphic case (M=1) — the added ways must not slow down the single-target fast path, which is the common case for direct-heavy code
- [ ] #4 jit == interp still holds and the differential suites pass (the IBTC is a dispatch optimization and must not be guest-visible)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
