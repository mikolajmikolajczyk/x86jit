---
id: TASK-278
title: >-
  perf: N-way IBTC — the per-site indirect-branch cache is monomorphic, so 2+
  targets cost 2.6x
status: To Do
assignee: []
created_date: '2026-07-22 07:09'
updated_date: '2026-07-22 09:32'
labels:
  - perf
  - jit
  - cranelift
  - dispatch
dependencies: []
priority: low
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
STEP 1 DONE (not the N-way cache itself) — probe the vcpu-private fast cache before `resolve` on the IBTC miss path.

Found while mapping the mechanism for the N-way work: `vm.rs` RET_IBTC_MISS called `resolve` DIRECTLY, skipping the `fast_get` probe the main dispatch loop uses. `resolve` is an RwLock read + HashMap lookup + clone + two shared atomic bumps; `fast_get` is a direct-mapped array index and a compare. Since the per-site IBTC holds one target, a multi-target site returns here on nearly every call, so that expensive path was the common case. The counter proved it: `indirect` reported fast_hits=0 despite ~940k dispatcher returns.

Also learned while reading: IBTC_MEGAMORPHIC_CAP = 8 (vm.rs). After 8 refills the dispatcher stops filling the slot entirely, so a 16-target site is not thrashing descriptors — it is running with a FROZEN one-entry cache, hitting 1/16. That bounds the descriptor leak today and must be re-thought together with any N-way design (a 4-way cache needs 4 refills just to warm up, burning half the budget, and again after every epoch flush).

MEASURED, alternating A/B, 5 iters, indirect workload (1M indirect calls, 16 targets):
  before: 53.13 / 55.48 ms      after: 32.74 / 32.49 ms      -40%
Per call 54.3 -> 32.6 ns; against the 13.3 ns monomorphic floor the miss overhead falls from ~41 ns to ~19 ns.
Counters confirm the mechanism rather than just the timing: indirect fast_hits 0 -> 937,449 (~94% of calls, exactly the frozen-cache miss rate), sqlite 28 -> 565, lua 21 -> 1037.

Soundness: the probe is keyed on the same rip, holds compiled blocks only, and is cleared on the same invalidation epoch as the outer loop's probe — it is the identical lookup, one call site earlier. It skips `drain_tier_up` and the hotness-gated tier-up on a hit, exactly as the outer probe already does.

Verification: cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' -> 891/891; clippy -D warnings clean; fmt clean.

REMAINING for this task: the N-way cache itself, still gated on AC#1 (measure the real per-site target distribution before choosing associativity). The 19 ns that survive are the dispatcher round-trip a hit still pays; an N-way cache would remove the round-trip entirely for sites within its associativity.

NEGATIVE RESULT FROM THE EMBEDDER (2026-07-22). unemups4 measured guest_exec in Celeste gameplay across three builds: 873563f (opt none) 23.9-25.3 ms vs 72674de (opt speed + the IBTC probe) 24.0-27.8 ms. NO CHANGE. Neither opt_level=speed nor the IBTC probe moved real guest code.

Why the IBTC work could not help there, from their counters: ~1,000,000 chained transfers per frame against ~5,000 fast_hits — indirect branches are about 0.5% of control transfers in that workload. The -40% I measured was on a bench doing 1,000,000 indirect calls; Celeste does 5,000 per frame. The optimization is sound and should pay on indirect-heavy runtimes (sqlite, lua both improved their fast_hits by 20-50x), but it targeted something that workload barely does.

METHODOLOGICAL LESSON, recorded so it is not repeated: the bench workload was chosen to isolate the mechanism, which made it a worst case rather than a representative one, and no check was made against a real profile BEFORE the work. The order should have been: profile the target workload, find what dominates, then optimize. Their profile says the dominant control-flow event is chained transfers, not indirect ones — now filed as TASK-280.

AC#1 (measure the real per-site target distribution) is unchanged and now doubly important: at 0.5% of transfers, an N-way IBTC has a hard ceiling on this workload regardless of how good it is. Anyone picking this up should first confirm there IS an indirect-heavy consumer, or deprioritize it.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
