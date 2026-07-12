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
- `x86jit-bench` — per-commit native/interp/jit timing + baseline gate (workloads today:
  sha256/fib, dispatch-heavy tiny blocks — NOT game-shaped yet).

## Tiers (priority order)

### Tier 0 — Measurement foundation (do FIRST; optimizing blind is waste)
- **task-147** (HIGH) — perf-bench v2: compile/run split, **native ratios**, noise-aware gate.
- **task-235** — game-shaped microbench suite (SIMD kernels, dispatch stress, hotloop sweep,
  memcpy, vtable dispatch). The harness that unblocks everything without a game.
- Profile the real SIMD binaries (openssl/bzip2/python) via task-196 perfmap + `perf` to
  seed the hot-op list.

### Tier 1 — Biggest lever: SIMD lowering (the ~223 helper→interp fallbacks)
- **task-236** — audit + rank the helper→interp SIMD fallbacks by games-hotness.
- **task-237** — native-lower the hot AVX/SSE float ops (drop helper→interp on the hot
  path) → NEON. Expect **2–10×** on SIMD-heavy loops. The single biggest games lever.

### Tier 2 — Dispatch overhead
- **task-146** (HIGH) — fix the bisected ~19% bg-tier dispatch regression (hot Vm/cache
  cache-line layout; profile, don't guess). Free win on dispatch-heavy code.
- IBTC + block chaining already landed; revisit quality if profiling flags computed-jump
  churn (game vtables / function pointers).

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

1. **task-147 + task-235** — measurement + game-shaped harness (unblocks the rest).
2. **task-146** — reclaim the known ~19% dispatch regression.
3. **task-236 → task-237** — audit then native-lower hot AVX/SSE float (the big lever).
4. **task-160 / 157** — hot-region OSR + region worker.
5. **task-103** — AOT cache for cold-start.

Every step: author/extend a microbench → measure (native ratio) → optimize → validate
bit-exact vs unicorn → CI-gate. Minutes-long iterations, no game required. The game, once
`unemups4` runs code, is added as a profiling source to discover title-specific hot spots
that then become new microbench entries.
