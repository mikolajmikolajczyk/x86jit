---
id: decision-5
title: >-
  Background tier-up publishes from the core dispatcher via cache.upgrade; the
  backend never touches the translation cache
date: '2026-07-06 18:45'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk (architect consult: Fable 5)

## Context

Background (concurrent) tier-up (`bg-tier` milestone, doc-27
`background-tier-plan.md`) moves hot-block compilation off the vcpu's critical
path: a block runs interpreted, a background worker compiles it, and the block is
then switched interp→compiled. That switch mutates the `TranslationCache`, which
lives in `x86jit-core` (`x86jit-core/src/cache.rs`) as a **private field of `Vm`**.
The compiler worker lives in `x86jit-cranelift` (the `Backend` impl owns the
`!Sync` `JITModule`; §15 forbids threads/channels leaking into core, whose deps
must stay exactly `{iced-x86}`).

The question: **who performs the cache mutation that publishes a finished
background compile?** Two shapes were on the table:

1. The backend worker holds a handle to the cache and calls `upgrade` itself when
   a compile finishes.
2. The backend only *produces* finished results; the **core dispatcher** drains
   them and publishes, reusing the existing `resolve`/`cache.upgrade` path.

## Decision

**Core drives the publish.** The `Backend` trait exposes `tier_up_finished() ->
Vec<TierUpFinished>` (plain data, no cache handle). Vcpus drain completions at the
top of `resolve` (`x86jit-core/src/vm.rs`) and publish each via the **existing**
epoch-guarded `cache.upgrade(pc, block, span, epoch)` (`cache.rs:116`) — the same
call the current synchronous tier-up already uses. The backend never sees, holds,
or mutates the `TranslationCache`.

Consequences of the boundary:

- The cache stays a **private `Vm` field** — no `Arc<TranslationCache>` restructure,
  no cross-crate cache handle, no second writer to reason about. The invalidation
  and SMC-race machinery (`upgrade`'s epoch check, `invalidate_overlapping`,
  the spans lock, #3) keeps its single owner.
- Publish latency is bounded: an interpreted hot block always routes through
  `resolve` (the R3 fast-resolve cache only holds `Compiled` entries), so a ready
  completion is picked up on that block's next dispatch — no extra poll point.
- `x86jit-core` keeps its dependency set (`{iced-x86}`, §15). The request/result
  types (`TierUpRequest`, `TierUpFinished`, `TierUpSubmit`) are plain structs in
  core; the queue, worker thread, and channel are entirely inside
  `x86jit-cranelift`.
- The epoch snapshot `resolve` already takes (`vm.rs:679`) rides on the request, so
  a compile that finishes after an SMC/munmap invalidation is rejected by the
  existing `upgrade` guard; the drain always clears the in-flight marker, so an
  unrelated-invalidation rejection self-heals by resubmission.

## Consequences

- **Positive:** one cache writer; zero new core deps; reuses proven publish +
  invalidation paths; the backend is a pure compile service (testable in isolation,
  and swappable — a future AOT/optimizing tier plugs into the same drain).
- **Cost:** completions are observed only at `resolve` (block dispatch), not the
  instant a compile lands — a negligible, bounded delay, and the correct trade for
  keeping the cache single-owner.
- **Bound:** if a future design needs the backend to publish other artifacts
  (e.g. FD-AOT persisted code), it must route through the same core-driven drain,
  not grow a cache handle — revisit this record if that constraint becomes costly.

## Alternatives considered

- **Backend holds a cache handle and publishes directly** — rejected: forces the
  cache behind `Arc` with a second cross-crate writer, duplicates the epoch/SMC
  race reasoning on the backend side, and leaks a core type into the compiler
  thread for no latency gain (the drain delay is bounded by design).
