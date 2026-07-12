---
id: doc-33
title: PS4-JIT performance roadmap
type: other
created_date: '2026-07-12 20:20'
---

# PS4-JIT performance roadmap

**Milestone:** `ps4-perf`. **Goal:** squeeze max JIT throughput for the x86-64-on-ARM
recompiler ahead of the PS4-emulator (`unemups4`) path — games are hot-loop + heavy-SIMD
+ draw-call-generation workloads. PS4 guests are x86-64 user-mode (Jaguar / Orbis), so
this is squarely x86jit's wheelhouse; no full-system work is required.

## Key principle: a running game is NOT a prerequisite

~80% of JIT-perf work is **game-agnostic** — any x86-64 game stresses the same machinery
(SIMD float, dispatch, hot loops). Validated by:
- **Correctness** → the unicorn oracle (real-CPU differential) — already in place.
- **Speed** → **game-shaped microbenchmarks** we author (matrix/vec math, memcpy, tight
  loops, vtable dispatch): deterministic, seconds-long, CI-gateable.

We also already have **real SIMD-heavy binaries** to profile now: openssl (crypto SIMD),
python, sqlite, bzip2, go_http. A game is the **last mile** (title-specific hot-spot
discovery + end-to-end frame time), not the development driver — developing perf against a
game is slower and noisier than microbench + oracle.

## Current infrastructure (what exists)

- Tiering: hotness counters + `region_decision` + `upgrade_region` + `try_begin_tier_up`
  (baseline→region promotion) — `x86jit-core/src/cache.rs`.
- Block chaining (task-65) + per-site **IBTC** for indirect branches (R4,
  `codegen/control.rs:51`).
- `perf`-map emission (task-196, done) for flamegraphs on real traces.
- `x86jit-bench` — per-commit native/interp/jit timing + baseline gate. **task-147 DONE**:
  bench v2 landed — native ratios (run/nat = honest "how far off native"), rolling-median
  series, load-aware gate (measures-but-doesn't-block under host load). Workloads today are
  sha256/fib (dispatch-heavy tiny blocks) — NOT game-shaped yet (that gap is task-235).

## Status note (already done)

- **task-147 DONE** — the measurement framework (native ratios, compile/run split,
  noise/load-aware gate) is in place. Tier 0 now only needs game-shaped *workloads*
  (task-235), not more bench framework.
- **task-146 DONE** — the "+19% bg-tier dispatch regression" was largely a *measurement
  artifact* (jit/interp ratio reads high under host load); closed by task-147's load-aware
  gate, not by a real 19% dispatch cost. So there is **no pending "reclaim 19%" win** — if
  future profiling shows real per-dispatch cost, file it fresh.

## Tiers (priority order)

### Tier 0 — Measurement foundation (framework DONE via task-147)
- **task-235** — game-shaped microbench suite (SIMD kernels, dispatch stress, hotloop sweep,
  memcpy, vtable dispatch). The harness that unblocks everything without a game. This is the
  remaining Tier-0 item now that the bench framework (task-147) has landed.
- Profile the real SIMD binaries (openssl/bzip2/python) via task-196 perfmap + `perf` to
  seed the hot-op list.

### Tier 1 — Biggest lever: SIMD lowering (the ~223 helper→interp fallbacks)
- **task-236** — audit + rank the helper→interp SIMD fallbacks by games-hotness.
- **task-237** — native-lower the hot AVX/SSE float ops (drop helper→interp on the hot
  path) → NEON. Expect **2–10×** on SIMD-heavy loops. The single biggest games lever.

### Tier 2 — Dispatch overhead
- IBTC + block chaining already landed (task-65, R4). The task-146 "+19% regression" was a
  load-artifact (see Status note), not a real cost. Revisit only if game-shaped profiling
  (task-235) flags real per-dispatch overhead or computed-jump churn (game vtables /
  function pointers) — file fresh with the profile.

### Tier 3 — Hot-region compilation (superblocks / traces)
- **task-160** — compiled backedge counters → baseline→region **OSR** promotion (jump into
  an optimized region mid-loop; games spend ~90% in a few loops).
- **task-157** — dedicated region-compile worker (heavy region compiles must not clog
  single-block tier-up).
- **task-158** — adaptive tier thresholds (scale the region gate with queue/code-cache
  pressure).
- **task-159** — hotloop-length sweep bench validating adaptive tier selection.

### Tier 4 — Persistent / AOT cache
- **task-103** — AOT / persistent translation cache. Games re-run the same (huge) code
  every launch; caching translations to disk kills cold-start re-JIT (first-run hitching).

### Tier 5 — Micro
- **task-238** — hot-path micro-opts: RAM bounds-check elision (provably in-region),
  guest-reg→host-reg residency across a block, lazy/elided EFLAGS coverage audit.

## Recommended start sequence

Bench framework (task-147) and the 146 "regression" are already resolved, so:

1. **task-235** — game-shaped microbench workloads on the existing bench-v2 framework
   (unblocks everything; also profile openssl/bzip2/python via task-196 to seed hot ops).
2. **task-236 → task-237** — audit then native-lower hot AVX/SSE float (THE big lever, 2–10×).
3. **task-160 / 157** — hot-region OSR + region worker.
4. **task-103** — AOT cache for cold-start.
5. **task-238** — hot-path micro-opts (RAM bounds-elision, reg residency, flags).

Every step: author/extend a microbench → measure (native ratio) → optimize → validate
bit-exact vs unicorn → CI-gate. Minutes-long iterations, no game required. The game, once
`unemups4` runs code, is added as a profiling source to discover title-specific hot spots
that then become new microbench entries.
