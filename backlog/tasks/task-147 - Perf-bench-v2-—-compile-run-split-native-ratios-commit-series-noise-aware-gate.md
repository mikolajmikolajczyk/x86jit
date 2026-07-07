---
id: TASK-147
title: >-
  Perf-bench v2 — compile/run split, native ratios, commit series, noise-aware
  gate
status: In Progress
assignee: []
created_date: '2026-07-07 08:57'
updated_date: '2026-07-07 09:26'
labels:
  - bg-tier
dependencies: []
priority: high
ordinal: 156000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Redesign x86jit-bench per doc-29 (backlog/docs/design/perf-bench-v2.md). Motivated by task-146: the pre-push gate blocked a clean push on a phantom sha256 +18% that was a stale baseline, not a regression — the ratio swings +-15% even at min-of-7 and the gate compares to one baseline point. Four gaps, all maintainer-requested: (1) compile time is fused into JIT run time (sqlite jit 1233ms is ~99% compile) — separate via instrumenting JitBackend::materialize (compile_ns counter) so run_ns = cold - compile; loop workloads also get a warm re-run cross-check. (2) no native comparison in gate/table — add jit/native, run/native, interp/native. (3) only the latest baseline is used — bench/history/<sha>.json is already a series; gate vs rolling median of last K clean baselines + a trend subcommand. (4) noise swamps signal — Stat{min,median,MAD,n} + warmup + loadavg/quality tag + a noise-aware gate (regression only if > max(threshold, c*MAD/median)). Full data-structure deltas, phases, and risks in doc-29. Approach approved by maintainer: instrument materialize for the split; rolling-median + noise-band for the gate.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 PB-1: Stat + warmup + noise-aware gate — the task-146 sha256 case no longer false-blocks across >=10 runs at varying load; performance.md shows median+-MAD
- [ ] #2 PB-2: compile_ns instrumented in JitBackend; run_ns = cold - compile reported; sqlite/lua run_ns is small (compile-dominated cold confirmed); x86jit-core stays {iced-x86}
- [ ] #3 PB-3: jit/native, run/native, interp/native columns where native exists (fib32 dashes)
- [ ] #4 PB-4: gate reference is the median of last K clean history baselines; trend subcommand prints the series; old history/ JSON still loads
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
PB-1 statistics core: Stat(min/median/MAD/n) + warmup + iters default + loadavg/quality in Record; table shows median+-MAD; noise-aware gate vs the single baseline (M5). Kills the task-146 false-positive class first. PB-2 compile/run split: JitBackend.compile_ns + Counters.compile_ns + run_ns + loop-workload jit_warm_ns; compile/run columns. PB-3 native ratios: jit/nat, run/nat, interp/nat + optional gate. PB-4 commit series: rolling-median reference from history/, trend subcommand, trend arrows in performance.md. Each phase lands independently; JSON format_version bump so the existing history/ series still reads.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
PB-2 landed (compile/run split). Backend::compile_ns() (core, default 0 — no dep added); JitBackend instruments Shared::compile_with (Instant around every foreground/tier-up/bg compile) into an AtomicU64. Counters gains compile_ns (read from vm.backend.compile_ns() post-run); WlResult.compile_ns wired; run() = jit_cold - compile (saturating). performance.md + console table gain compile + run columns. VALIDATION: sqlite cold 1181ms = compile 1188ms, run 0.00ms (JIT'd code runs instantly — ~100% compile, the one-shot truth); sha256 cold 18.4ms = compile 10.5ms + run 7.9ms (true JIT sha256 = 7.9ms vs interp 219ms ~27x, hidden before); fib32 run~=cold (tiny blocks, ~0 compile). Warm re-run (jit_warm_ns) skipped — not in AC, needs Guest reset-keep-cache; run_ns split delivers the value. 9/9 JIT tests pass, clippy+fmt clean, core stays {iced-x86}. TODO PB-3 (native ratios), PB-4 (rolling-median series + trend).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
- [ ] #4 cargo nextest run (--features unicorn) green minus fuzz_robustness
- [ ] #5 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #6 cargo fmt --check clean
<!-- DOD:END -->
