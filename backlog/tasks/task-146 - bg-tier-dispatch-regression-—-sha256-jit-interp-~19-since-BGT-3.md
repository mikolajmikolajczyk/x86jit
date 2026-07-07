---
id: TASK-146
title: bg-tier dispatch regression — sha256 jit/interp ~+19% since BGT-3
status: To Do
assignee: []
created_date: '2026-07-07 07:55'
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

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
