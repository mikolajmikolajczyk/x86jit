---
id: doc-27
title: Background tier-up — execution plan (BGT)
type: specification
created_date: '2026-07-06 18:17'
---

# Background tier-up — execution plan (BGT)

Ready-to-execute plan for moving hotness-gated tier-up (FD-TIER, task-106) off
the vcpu's critical path: a hot block keeps running interpreted while a
**background compiler thread** produces its compiled form, which is then
atomically switched in through the existing `TranslationCache::upgrade`
machinery. Standard concurrent tiering (V8 / HotSpot C1-C2 shape). Authored by
Fable 5 (architect session, 2026-07-06), grounded in the code at commit
`504e97d`; exact sites cited below.

**Framing.** This is a JIT-architecture improvement (§9 dispatcher, §8
backends): it removes the inline-compile latency spike on the nth execution of
a hot block and opens the door to a future optimizing tier (expensive compiles
off the critical path — see superblock-plan.md T3f "future path"). It is
**orthogonal to task-134** (virtual-monotonic threaded clock): background
tier-up smooths compile stalls, it does not fix guest-visible time racing the
host. It complements FD-AOT (aot-plan.md), which attacks compile cost by
persisting; this attacks *where* the cost is paid.

## Current shape (verified sites)

- Tier-up compiles **inline** in `resolve` (`x86jit-core/src/vm.rs:673-700`):
  epoch snapshot → `cache.get` → `bump_hotness(pc) >= thr` →
  `vm.materialize(ir)` **on the vcpu thread** → `cache.upgrade(pc, compiled,
  span, epoch)`.
- Deferred materialization: `finish_single` (`vm.rs:740-757`) caches fresh
  blocks as `CachedBlock::Interpreted(Arc<IrBlock>)` when `tier_up_after` is
  `Some`.
- The atomic interp→compiled switch **already exists and is race-safe**:
  `TranslationCache::upgrade` (`x86jit-core/src/cache.rs:116-130`) commits
  under the spans+map write locks and rejects when the invalidation epoch moved
  since the caller's snapshot (the #3 race; unit-tested at `cache.rs:291-330`).
- Hotness: `bump_hotness` (`cache.rs:94-105`), read-lock fast path.
- The backend boundary: `trait Backend` (`vm.rs:31-74`), `materialize(&self)`;
  the Cranelift impl owns its `JITModule` behind `Mutex<Jit>`
  (`x86jit-cranelift/src/lib.rs:181-235`).

## Header decisions (settled — the design)

### D1 — Trait boundary: two default-implemented `Backend` methods, types in core

`x86jit-core` gains **types and trait methods only** — no threads, no
channels, deps stay exactly `{iced-x86}` (§15). In `vm.rs` next to
`trait Backend`:

```rust
pub struct TierUpRequest {
    pub pc: u64,
    pub ir: Arc<IrBlock>,           // clone of the cached Arc — cheap
    pub consistency: MemConsistency,
    pub mmio: Option<(u64, u64)>,
    pub span: (u64, u32),           // (ir.guest_start, ir.guest_len)
    pub epoch: u64,                 // resolve's snapshot (vm.rs:679)
}
pub struct TierUpFinished {
    pub pc: u64,
    pub block: CachedBlock,
    pub span: (u64, u32),
    pub epoch: u64,                 // echoed from the request
}
pub enum TierUpSubmit { Queued, Busy, Unsupported }

trait Backend {
    // ... existing ...
    fn tier_up_async(&self, req: TierUpRequest) -> TierUpSubmit {
        let _ = req; TierUpSubmit::Unsupported
    }
    fn tier_up_finished(&self) -> Vec<TierUpFinished> { Vec::new() }
}
```

`Unsupported` → core falls back to today's inline compile (a backend that
never implements async still tiers up correctly). `Busy` (bounded queue full)
→ the block **stays interpreted** and retries on a later execution — never an
inline compile spike exactly when compile pressure is highest. `Vec::new()`
does not allocate, so the drain probe is free for non-async backends.

### D2 — Publish is driven by core (vcpus drain; backend never touches the cache)

The `TranslationCache` stays a private field of `Vm` — no `Arc` restructure,
no backend→cache handle, no self-referential `Vm`. Completions queue up inside
the backend; **any vcpu drains them at the top of `resolve`** (gated on the
background flag) and publishes each via the existing
`upgrade(pc, block, span, epoch)`:

```rust
// top of resolve (vm.rs:673), before the lookup loop:
if vm.tier_up_background {
    for done in vm.backend.tier_up_finished() {
        if vm.cache.upgrade(done.pc, done.block, done.span, done.epoch)
            { /* count published */ } else { /* count rejected */ }
        vm.cache.end_tier_up(done.pc);   // always — see D4/D5
    }
}
```

Latency bound: the hot block being tiered is *interpreted*, and interpreted
blocks always route through `resolve` (the R3 fast-resolve cache only holds
`Compiled` entries — `vm.rs:527-543`), so its own next execution drains the
queue. A completion for a block the guest never re-executes may sit queued
until some other `resolve` call; harmless (the module owns the code memory
regardless). Multi-vcpu: drain pops under a mutex, each completion is
published exactly once by whoever popped it. Rejected alternative: the
compiler thread publishes directly — requires `Vm.cache: Arc<TranslationCache>`
plus an attach-time handle, couples the backend to core internals, and
complicates `fork_with_backend` (`vm.rs:147-155`) for no latency win that
matters.

### D3 — Compiler thread: single worker owned by the backend, module stays put

`JitBackend` restructures to `{ shared: Arc<Shared>, .. }` where `Shared`
holds the existing `Mutex<Jit>` (module + fbctx + slots,
`x86jit-cranelift/src/lib.rs:188-205`), the bounded request queue, the
completion queue, and an `AtomicUsize` ready-count. The worker thread owns a
clone of the `Arc` and loops: recv request → lock `Mutex<Jit>` →
`compile`/`compile_with` (unchanged, `lib.rs:288-378`) → push
`TierUpFinished` → bump ready-count. This satisfies the `JITModule: !Sync` /
`finalize_definitions(&mut)` constraint the same way the code already does —
one mutex — and keeps the synchronous `materialize` path (eager mode, region
compiles, the `Unsupported` fallback) working unchanged and correctly
serialized against the worker. `Jit` is already `Send` (today's
`JitBackend: Sync` via `Mutex<Jit>` proves it).

- **Lazy spawn** on first `tier_up_async` (eager-mode users never pay a
  thread), guarded by a `Mutex<Option<JoinHandle>>` in `Shared` held only at
  spawn/drop, never per-request.
- **Clean join**: `impl Drop for JitBackend` sets a shutdown flag / closes the
  channel, wakes the worker, joins. `Vm` field order (`mem, cache, backend`)
  drops the cache (pointer holders) before the backend (code owner) — same
  invariant as today (§9.1 ownership note). A worker panic must **not**
  re-panic in `Drop` (swallow the `JoinHandle` error); the observable effect
  of a dead worker is "blocks stay interpreted" — slow but correct.
- **Single thread, not a pool.** One `JITModule` = one mutex; a pool buys
  nothing until FD-AOT B0.2 retires `JITModule` for a raw arena
  (aot-plan.md) — per-thread `Context`s over a lock-free arena become natural
  then. Note the synergy; do not build it now.
- Threading stays **std-only** (`std::thread`, `std::sync::mpsc::sync_channel`
  or `Mutex<VecDeque>+Condvar`) — no new deps in `x86jit-cranelift` either.
- Test/embedder hook: `JitBackend::tier_up_handle() -> TierUpHandle` (an
  `Arc<Shared>` clone, grabbed before boxing) with `wait_idle()` — blocks
  until the request queue is empty and the worker is not mid-compile. This is
  the determinism lever for tests (D6).

### D4 — Dedup / backpressure: an in-flight set in the cache, a bounded queue in the backend

A hot block must be enqueued once, not on every execution while its compile is
in flight. The marker lives in `TranslationCache` next to `hotness` (it shares
its lifecycle):

```rust
tier_pending: Mutex<HashSet<u64>>,
pub fn try_begin_tier_up(&self, pc) -> bool  // insert; false if already pending
pub fn end_tier_up(&self, pc)                // remove (idempotent)
```

`invalidate_overlapping` (`cache.rs:235-270`) additionally removes every
victim from `tier_pending` (a dropped block's re-lifted successor must be able
to tier up again). **Lock order** extends the existing one: spans → map →
hotness → tier_pending; `try_begin/end` take only the pending lock.
Hot-path change in `resolve` (replacing the inline compile at `vm.rs:688-699`
when background mode is on):

```rust
if vm.cache.bump_hotness(pc) >= thr && vm.cache.try_begin_tier_up(pc) {
    match vm.backend.tier_up_async(TierUpRequest { .. epoch .. }) {
        TierUpSubmit::Queued => {}
        TierUpSubmit::Busy => vm.cache.end_tier_up(pc),       // retry later
        TierUpSubmit::Unsupported => { vm.cache.end_tier_up(pc);
            /* fall through to today's inline materialize+upgrade */ }
    }
}
return Ok(block);   // keep interpreting until the switch lands
```

Two vcpus racing `try_begin_tier_up` is settled by the set. Queue capacity
~64 requests (tune in BGT-5); `bump_hotness` keeps counting past `thr`, so a
`Busy` block re-attempts on its next execution. (Nit, non-blocking: the
`AtomicU32` counter wraps after 2^32 interpreted executions — make it
saturate while touching this code, one line.)

### D5 — Invalidation while in flight: reuse the epoch machinery unchanged

The request carries the **same epoch snapshot `resolve` already takes**
(`vm.rs:679`) — bit-for-bit the inline path's race guard, only the window is
wider (submit→publish spans the whole compile). Cases:

- **SMC / unmap drops the block mid-compile** (§10): `invalidate_overlapping`
  bumps the epoch and clears `tier_pending[pc]`; the stale completion is
  rejected by `upgrade` (a would-be resurrection of a spanless block — exactly
  the #3 test, `cache.rs:309-330`). The re-lifted block re-heats and
  resubmits; if the old completion is still queued, both drain and the epoch
  check picks correctly.
- **Unrelated invalidation** (any epoch bump, e.g. a Trap-region `Vm::map`
  flushing everything, `vm.rs:198-204`): `upgrade` rejects conservatively.
  Because the drain **always** calls `end_tier_up` after the publish attempt,
  the still-live block re-heats and resubmits with a fresh epoch. Watch the
  `tier_bg_rejected` counter; a per-span epoch is a possible later refinement,
  not v1.
- **Publish success**: `upgrade` re-establishes the span and drops the hotness
  entry (existing behavior); the drain clears `tier_pending`. Inbound chaining
  needs nothing new: a compiled predecessor's `RET_LINK` re-resolve now sees
  `Compiled` and links (`vm.rs:578-597`); the R3 fast cache starts hitting.

Soundness note (same argument as inline): if the epoch is unchanged at
`upgrade`, the entry at `pc` is necessarily still the same interpreted block —
an entry can only disappear or change via an invalidation, which bumps the
epoch under the very locks `upgrade` holds.

### D6 — Opt-in surface, default off, deterministic tests

- `Vm::set_tier_up_background(bool)` (field beside `tier_up_after`,
  `vm.rs:126`; inherited by `fork_with_backend`). Default **false**; only
  meaningful when `tier_up_after` is `Some` **and** the backend supports async
  (else the `Unsupported` fallback silently gives today's inline behavior).
- Same stance as task-106: the differential + fuzz corpus stays on the
  deterministic configs; interp and compiled code are semantically identical,
  but the corpus must not depend on *when* the switch lands. Full-corpus
  background sweeps are env-gated (`X86JIT_BG_TIER=1`), mirroring the
  `X86JIT_SUPERBLOCKS=1` precedent (superblock-plan.md T3b).
- `x86jit-tests/src/guest.rs` builder gains `.tier_up_background()`
  (alongside `.tier_up(Some(n))`, `guest.rs:150`).
- Observability (testing.md §8.2 "fires" axis): cache counters
  `tier_bg_published` / `tier_bg_rejected` (+ accessors), styled after
  `chained`/`regions`/`ibtc_filled` (`cache.rs:184-201`).
- Deterministic test recipe: build with a low threshold + background on, grab
  `tier_up_handle()` before boxing the backend → run the hot block ≥ thr times
  (still interpreted; assert pending) → `handle.wait_idle()` → run once more
  (that dispatch's `resolve` drains and publishes) → assert
  `tier_bg_published == 1` and state/output equals the interpreter oracle. No
  sleeps, no timing.

## Block state machine (reference)

```
Cold interp ──bump_hotness ≥ thr, try_begin ok──▶ Pending (queued, epoch e)
Pending ──compile done, drain, upgrade ok───────▶ Compiled  (pending−, hotness−)
Pending ──drain, upgrade rejected (epoch≠e)─────▶ Hot interp (pending−) → resubmits
Pending ──invalidated (victim)──────────────────▶ gone (pending−, hotness−); a
                                                  stale completion is later
                                                  rejected by the epoch check
Pending ──submit Busy───────────────────────────▶ Hot interp (pending−) → retries
```

## Phases (risk-ordered, each independently landable + testable)

### BGT-1 (task-135) — core vocabulary (inert)
`TierUpRequest`/`TierUpFinished`/`TierUpSubmit` + the two defaulted `Backend`
methods (`vm.rs`); `tier_pending` set + `try_begin_tier_up`/`end_tier_up` +
victim clearing in `invalidate_overlapping`; `tier_bg_published/rejected`
counters (`cache.rs`). **No behavior change** — nothing calls any of it yet.
Gate: full suite unchanged; new cache unit tests for every state-machine
transition above (incl. invalidation clearing the pending set).

### BGT-2 (task-136) — background compiler thread in `x86jit-cranelift`
`Arc<Shared>` restructure, bounded request queue, worker loop compiling under
the existing `Mutex<Jit>`, completion queue + ready-count fast path, lazy
spawn, `Drop` join (no re-panic), `TierUpHandle::wait_idle`. Implements
`tier_up_async`/`tier_up_finished` on `JitBackend`. Gate: crate-local tests —
submit a hand-built `IrBlock` → `wait_idle` → drain yields a `Compiled` block
that executes; `Busy` on a full queue; drop with queued requests joins
cleanly; eager `materialize` still works concurrently (mutex serialization).

### BGT-3 (task-137) — dispatcher wiring + opt-in (the feature lands)
`resolve` drain/publish (D2) + hot-path submit (D4) + `Unsupported` inline
fallback; `Vm::set_tier_up_background` + fork inheritance; guest-builder knob.
Gate: the D6 deterministic test; a real-program run background-on asserting
output identical to interp and `tier_bg_published > 0`; env-gated
`X86JIT_BG_TIER=1` differential sweep green; default-off suite untouched.

### BGT-4 (task-138) — invalidation-in-flight + concurrency hardening
Targeted races, using `wait_idle` to sequence deterministically: SMC write to
the hot block's page while its compile is queued → publish rejected, block
re-lifts, re-tiers; Trap-region `map` mid-flight (epoch bump via full flush) →
rejected then resubmitted; duplicate completions for one pc (invalidate +
re-heat while the old request is queued); threaded driver
(`x86jit-linux/src/thread.rs`, shared `Arc<Vm>`) with background on —
multi-vcpu drain, output equality. Fix whatever these force.

### BGT-5 (task-139) — surface completion, bench, docs
`x86jit-bench`: background mode next to `tier_from_env()`
(`workloads.rs:97,275`); measure inline vs background on sqlite/lua/go-startup
(wall time + max single-dispatch stall if cheap to record); record a perf
snapshot. `x86jit-run` (`lib.rs:275`): env knob, default stays inline pending
the numbers. Tune queue capacity. Update `backlog/docs/status.md` +
`architecture.md`; note in `deferred.md` what stayed out (pool, region
tier-up, per-span epoch).

### BGT-6 (task-140) — (scope expansion, deferred) background *region* tier-up
Hotness-gated superblock formation compiled in the background — the
superblock-plan T3f "future path to default-viability" (region compile is too
heavy inline even when hot). Needs: the tier-up trigger runs `lift_region` and
submits an `IrRegion` request; the publish is a multi-span `upgrade`. Filed as
its own task; **not** part of BGT-1..5.

## Risks

- **Wider epoch window** → publish rejections under mapping/SMC churn;
  self-healing (resubmit) but wasted compiles. Bounded by the
  `tier_bg_rejected` counter; escalate to a per-span epoch only if real
  workloads show it.
- **Worker vs eager compiles share one mutex**: a long region compile delays
  background publishes (and vice versa). Same total serialization as today;
  becomes solvable after FD-AOT B0.2.
- **Nondeterminism leaking into the oracle corpus** — guarded by default-off,
  env-gated sweeps, and the `wait_idle` recipe for the deterministic tests.
- **Drop-order regressions**: `CompiledPtr`s in the completion queue outlive
  nothing (the queue lives in `Shared`, which dies with the backend, after the
  cache); keep the `Vm` field order and say so in a comment.
- **aarch64**: nothing new — publication goes through the existing
  `upgrade`/epoch fences and the `Relaxed`/`Release` slot protocol
  (`vm.rs:578-635`) unchanged; still verify with the manual ARM CI workflow.

## Open decisions for the maintainer

1. **Milestone placement**: tasks filed under a new `bg-tier` milestone
   (mirrors `go-caddy`/`code-review` as a coherent track) vs folding into
   `open-backlog` beside FD-TIER/FD-AOT.
2. **Decision record**: D2 ("publish driven by core; the backend never touches
   the cache") is architecture-grade — worth a `backlog decision create` per
   decisions.md. Recommended; maintainer's call to author it.
3. **`Busy` policy** (stay interpreted, D4) and the queue-capacity default (64).
4. **`x86jit-run` default** once BGT-5 numbers exist: keep inline, or flip the
   runner to background.
