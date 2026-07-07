---
id: TASK-146
title: bg-tier dispatch regression — sha256 jit/interp ~+19% since BGT-3
status: Done
assignee: []
created_date: '2026-07-07 07:55'
updated_date: '2026-07-07 08:31'
labels:
  - bg-tier
dependencies: []
priority: high
ordinal: 155000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Perf gate (baseline 9cf3fba) blocks on sha256 jit/interp ~+19% (samples 11-26%). Bisected cleanly to bg-tier: ccb5444 (pre-bg-tier) +0.2%, f44406e (post-BGT-3) +21.2%, 5fe4e8b (VCLK-2) +22.1% — VCLK added nothing; cost is BGT-1..3. Metric is a jit/interp RATIO, so a per-dispatch cost hurts JIT (fast blocks) far more than interp; sha256 is dispatch-heavy (tiny blocks) so trips hardest, fib32 was +8.6% (under threshold). The only visible BGT-3 fast-path addition is a single if-tier_up_background bool branch (drain gated on it; the bg-submit block is in the tier-up path, not the eager fast path) — implausibly small for ~19%, so the cause is likely deeper: struct/cache-line layout shifted by BGT-1 new fields (Vm.tier_up_background, TranslationCache.tier_pending Mutex+HashSet, bg counters) pushing hot fields (backend/cache/mem) across cache lines, or the epoch Acquire load landing colder. Needs profiling (perf/cachegrind on resolve/the dispatch loop), not a guess. NB: the push that surfaced this was overridden with X86JIT_ALLOW_PERF_REGRESSION=1 (regression is pre-existing bg-tier, not the VCLK work in that push).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Root-caused with a profile (perf/cachegrind on resolve and the dispatch loop) — name the actual cost, not a guess
- [ ] #2 sha256 jit/interp back within 10% of the 9cf3fba baseline, or a deliberate re-baseline with the bg-tier tradeoff recorded
- [ ] #3 fib32 + other hot workloads not regressed; differential corpus green
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
RESOLVED — NOT a real regression; the perf baseline was stale. Root-caused with a drift-canceling interleaved A/B (HEAD vs a ccb5444 worktree, binaries run back-to-back per round, 5 rounds): sha256 jit/interp ratio HEAD [0.104,0.110,0.111,0.105,0.098] vs ccb5444 (pre-bg-tier) [0.103,0.106,0.105,0.104,0.110] — statistically identical (r5 even has ccb HIGHER). So pre-bg-tier and HEAD measure the same ~0.105 ratio; the old baseline (0.089 @ 9cf3fba) drifted ~+18% on this machine, hitting ALL commits equally, not just bg-tier. The original single-sample bisect (ccb +0.2% / f44406e +21%) was noise — the gate ratio swings 0.098-0.111 (±15%) between invocations even min-of-7. Mechanism confirmation: sha256 runs almost entirely CHAINED (record counters: chained=460069, misses=19), so its JIT hot loop never re-enters resolve()/the dispatcher — bg-tier's resolve changes cannot touch it. FIX: re-recorded the baseline at c18b941 (sha256 10.06x jit/int); gate now passes 3/3. The earlier push override (X86JIT_ALLOW_PERF_REGRESSION=1) was correct — there was no real regression. NB the gate is inherently load/thermal-sensitive on this machine (ratio ±15%); a follow-up to harden it (more iters / discard-under-load) may be worth it but is out of scope here.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
