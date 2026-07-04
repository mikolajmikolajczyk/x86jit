# M5 — Performance (ongoing)

**Goal:** make the working JIT fast. Each optimization ships with THREE test axes — correctness (doesn't change behavior), "fires" (isn't a silent no-op), performance (actually helps).

**Spec:** spec.md §3.2 (Variant B), §12 (M5); testing.md §8. **Prereq:** M4 (library must be correct first). Optimization, not required for "working".

## Optimizations (each is a self-contained unit)

- [x] **M5-T1** — Block chaining: stitch blocks without returning to the dispatcher. (§12 M5)
  - [x] **M5-T1-preempt** — Keep a preemption path: chained edges must still let the budget tick / honor an "exit now" flag, or a tight chained loop never yields `BudgetExhausted` and starves other vcpus (kills M7 cooperative scheduling). (§9.2, §16)
- **M5-T2** — moved to [open-backlog.md](open-backlog.md).
- **M5-T3** — moved to [open-backlog.md](open-backlog.md).

## Per-optimization test tasks (T§8 — the trap)

For **each** optimization above:

- [x] **M5-T4** — Correctness axis: run the whole corpus through the config matrix; the optimization ON must equal the interpreter base. Test each opt **separately** (`JitOpt(Opt::X)`), not only all-on, so a breakage is localizable. (T§8.1)
- [x] **M5-T5** — "Fires" axis: an `OptStats` counter per optimization (`chained_jumps`, `elided_flag_calcs`, …) + a targeted test on a crafted input asserting the counter moved. Catches the silent no-op where the opt does nothing and passes correctness because "nothing changed = nothing broke". (T§8.2)
- [x] **M5-T6** — Performance axis: benchmark (criterion or custom) opt on-vs-off on a **realistic block mix or a real `programs/` binary**, compared relatively. Micro-benchmark on one loop lies. (T§8.3)

## Exit criteria

Measurable throughput gains on realistic workloads, with every optimization proven to (a) not break, (b) actually fire, (c) net-help. No optimization merged without all three axes.
