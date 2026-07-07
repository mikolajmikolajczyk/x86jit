---
id: doc-29
title: 'Perf-bench v2 — compile/run split, native ratios, commit series, noise-aware gate'
type: specification
created_date: '2026-07-07'
---

# Perf-bench v2

Redesign of `x86jit-bench` (`record`/`gate`/`experiment` + `performance.md`) so the
numbers are trustworthy and informative. Motivated by task-146: the pre-push gate
blocked a clean push on a phantom **sha256 jit/interp +18%** that a drift-canceling
interleaved A/B proved to be a **stale baseline**, not a code regression — the ratio
swings ±15% between invocations even at min-of-7, and the gate compares to a single
baseline point. Four gaps to close, all requested by the maintainer:

1. **Compile time is fused into JIT run time** — `sqlite` "jit 1233 ms" is ~99%
   compilation (compile-every-block one-shot), hiding the JIT's real steady-state
   speed. Separate them.
2. **No native comparison in the gate/table** — `native_ns` is measured but only
   `jit/interp` is reported. Report `jit/native`, `run/native`, `interp/native`.
3. **Only the latest baseline is used** — `bench/history/<sha>.json` is already a
   per-commit series, but `gate` compares to one `baseline.json`. Use the series.
4. **Noise swamps the signal** — min-of-N is not enough; a ±15% metric with a 10%
   threshold false-positives. Need statistics + a noise-aware threshold.

## Current shape (verified sites)

- `workloads.rs`: `Workload { name, kind, guest: fn(Box<dyn Backend>)->(Vec<u8>,
  Counters), native: Option<fn()->Vec<u8>>, expect }`; `Counters { chained,
  ibtc_filled, fast_hits, misses }`. Four workloads: `fib32` (dispatch-micro, no
  native), `sha256` (compute-hot), `sqlite`/`lua` (one-shot).
- `main.rs`: `time_it(iters, f)` returns **min-of-N** and the first output;
  `run_workloads` times interp + JIT (whole guest run, compile+exec fused) + native;
  `record` writes `history/<sha>.json` + `baseline.json` + `performance.md`; `gate`
  measures HEAD, compares each workload's interp & jit time to `baseline.json`, exits
  1 if any is > `X86JIT_PERF_THRESHOLD` (default 10%) slower.
- `report.rs`: `history_dir()` (`bench/history/`), `save`/`load`, `save_baseline`/
  `load_baseline` (single `bench/baseline.json`), `write_performance_md(rec, prev)`.
- `WlResult { name, kind, native_ns, interp_ns, jit_ns, chained, ibtc_filled,
  fast_hits, misses }` — the stored per-workload record.

## The model

### M1 — Statistics per timing (kills the ±15% false-positive at the source)

`time_it` returns a **distribution summary**, not one number:

```rust
struct Stat {
    min_ns: u64,     // fastest sample (intrinsic-cost estimate, kept)
    median_ns: u64,  // the gate's reference point (robust central tendency)
    mad_ns: u64,     // median absolute deviation — the noise band
    n: u32,          // samples kept (after warmup discard)
}
```

- **Warmup**: discard the first `W` samples (default 2) — cold I-cache / page faults /
  frequency ramp. Configurable `--warmup`.
- **Default iters up** from a handful to e.g. 15 (min-of-N and MAD both stabilize).
- **Machine-quality capture**: record `loadavg1` (from `/proc/loadavg`) and a
  `quality: clean | loaded | dirty` tag in the `Record`. A record taken at
  `loadavg > cores*0.5` is tagged `loaded`; `gate`/`record` warn, and a `loaded`
  record is **not** eligible to become a rolling-median reference (M4).

### M2 — Compile vs run split (instrument `materialize`)

Add compile-time accounting to `JitBackend` (interior `AtomicU64 compile_ns`, bumped
by an `Instant` around `materialize` + `materialize_region`), exposed to the bench.
Plumbing (keeps `x86jit-core` at `{iced-x86}`): the counter lives in
`x86jit-cranelift`; `Counters` gains `compile_ns`, populated from the JIT backend on
the guest run the same way cache counters are read today (the bench constructs the
`JitBackend`, so it can read a getter directly — no core change needed).

Derived per workload:

- `jit_cold_ns` = today's `jit_ns` (compile + execute, the real end-to-end cost).
- `compile_ns` = summed `materialize` time during that run.
- `run_ns` = `jit_cold_ns − compile_ns` — the JIT's execution cost with compilation
  removed (the number that matters for a long-running server; `sqlite`'s will finally
  be small).

For **loop workloads** (`dispatch-micro`, `compute-hot`) also measure `jit_warm_ns` —
re-enter the guest a second time on the **same `Vm`** (cache already warm, zero
compiles) — as an independent cross-check on `run_ns`. One-shots (`sqlite`, `lua`)
exit and can't be cheaply re-run with warm state, so they rely on the instrumented
`run_ns` only (documented per-workload via `kind`).

### M3 — Ratios vs native

Report and (optionally) gate on, where `native_ns` exists:

- `jit/native` (`jit_cold` — includes compile), `run/native` (steady-state), and
  `interp/native`. `fib32` has no native (hand-assembled snippet) → dashes.

The headline "how good is the JIT" number becomes `run/native` (compile amortized),
with `jit/native` showing the cold penalty.

### M4 — Commit series + rolling-median reference

`bench/history/<sha>.json` is already the series; make `gate` and the table use it.

- **Reference = median of the last `K` clean baselines** (default `K=5`), per
  workload per metric — not a single point. A lone noisy record can no longer set a
  bad ratchet, and a single noisy HEAD measurement is compared against a stable
  reference. `baseline.json` stays as the *pointer* to the accepted ratchet head;
  the rolling window is read from `history/`.
- **`trend` subcommand**: print the last `N` commits' key metrics per workload
  (sparkline-ish table) so drift is visible, not surprising.
- `performance.md` gains a **trend arrow** over the last `N` (▲/▼/~) beside the Δ.

### M5 — Noise-aware gate

A workload/metric regresses only if HEAD's **median** is worse than the reference
median by **more than `max(threshold, noiseband)`**, where
`noiseband = c · MAD/median` (both current and reference MAD; `c≈3`). So a ±15%
intrinsic-noise metric needs a >±15% shift to trip — no more phantom blocks. Gate
report shows, per workload: `ref_median`, `head_median ± MAD`, `Δ%`, `noiseband%`,
verdict. `X86JIT_PERF_THRESHOLD` and a new `X86JIT_PERF_MIN_SAMPLES` stay tunable;
`X86JIT_ALLOW_PERF_REGRESSION=1` still overrides.

### M6 — The `performance.md` table (the "pro stats")

Per workload:

| col | meaning |
|-----|---------|
| native | host reference (— if none) |
| interp | interpreter median ± MAD |
| jit-cold | compile + execute |
| compile | compilation only (M2) |
| run | steady-state execute (`cold − compile`) |
| jit/nat · run/nat · interp/nat | vs native (M3) |
| jit/int | legacy ratio (kept) |
| Δ vs prev · trend | signed delta + arrow over last N (M4) |

## Data-structure deltas

- `Counters` += `compile_ns: u64`.
- `WlResult`: `interp_ns → interp: Stat`, `jit_ns → jit_cold: Stat`, `+ compile_ns`,
  `+ run_ns` (derived), `+ jit_warm_ns: Option<Stat>`, `native_ns → native:
  Option<Stat>`. Keep the counter fields.
- `Record` += `loadavg1: f64`, `quality: enum`.
- `JitBackend` += interior `compile_ns` accumulator + a getter.

Back-compat: bump a `format_version` in the JSON; `load` tolerates old records
(missing fields default), so the existing `history/` series still reads.

## Phases

- **PB-1 — statistics core.** `Stat` (min/median/MAD/n) + warmup + iters default +
  loadavg/quality in `Record`. Table shows median±MAD. Gate still single-baseline but
  noise-aware (M5) against it. Immediately kills the task-146 false-positive class.
- **PB-2 — compile/run split.** `JitBackend.compile_ns` + `Counters.compile_ns` +
  `run_ns` + loop-workload `jit_warm_ns`. Table gains compile/run columns.
- **PB-3 — native ratios.** `jit/nat`, `run/nat`, `interp/nat` in table + optional gate.
- **PB-4 — commit series.** Rolling-median reference from `history/`; `trend`
  subcommand; trend arrows in `performance.md`.

Each phase lands independently, keeps `record`/`gate` working, and re-reads the
existing `history/` series.

## Risks

- **R1 warm re-run on one-shots** — infeasible without a reset-state-keep-cache
  primitive on the `Guest` harness; scoped out (instrumented `run_ns` covers them).
  Revisit only if `run_ns` proves untrustworthy for one-shots.
- **R2 rolling median needs enough clean history** — with `< K` clean records, fall
  back to the single accepted baseline; log which reference was used.
- **R3 MAD-based noiseband could mask a real small regression** — accepted: the corpus
  is for catching *gross* regressions; a sub-noise regression is below the tool's
  resolution on this hardware anyway. A quieter reference host (CI) tightens it later.
- **R4 instrumenting `materialize` perturbs its own timing** — an `Instant` pair per
  block is ~tens of ns vs a ~µs compile; negligible, and it is excluded from `run_ns`
  by construction (only added to `compile_ns`).
