---
id: TASK-136
title: >-
  BGT-2 — background compiler thread in x86jit-cranelift (queue, worker,
  lifecycle, wait_idle)
status: Done
assignee: []
created_date: '2026-07-06 18:22'
updated_date: '2026-07-06 19:10'
labels: []
milestone: m-0
dependencies:
  - TASK-135
ordinal: 145000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Phase 2 of background-tier-plan.md (doc-27, D3). All threading lives in the backend crate; core stays thread-free.

- Restructure JitBackend to { shared: Arc<Shared>, .. }: Shared holds the existing Mutex<Jit> (module+fbctx+slots, x86jit-cranelift/src/lib.rs:188-205), a bounded request queue (~64, std-only: mpsc::sync_channel or Mutex<VecDeque>+Condvar), a completion queue, and an AtomicUsize ready-count (fast empty probe).
- Worker loop: recv -> lock Mutex<Jit> -> compile via the existing compile/compile_with (lib.rs:288-378) -> push TierUpFinished -> bump ready-count. JITModule is !Sync / finalize needs &mut — the shared mutex satisfies it exactly as today, and keeps synchronous materialize (eager mode, regions, Unsupported fallback) working, serialized against the worker.
- Lazy spawn on first tier_up_async; Drop signals shutdown, wakes the worker, joins — never re-panics on a poisoned/panicked worker (a dead worker means blocks stay interpreted: slow but correct).
- Implement Backend::tier_up_async (Queued/Busy) and tier_up_finished (drain, ready-count-gated) on JitBackend.
- JitBackend::tier_up_handle() -> TierUpHandle (Arc<Shared> clone) with wait_idle() — the determinism lever for tests (grab before boxing the backend).
No new external deps (std threading only).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Crate-local test: submit a hand-built IrBlock request, wait_idle, tier_up_finished yields a Compiled block that executes correctly
- [ ] #2 Busy returned on a full queue; queued requests still complete
- [ ] #3 Drop with requests queued and mid-compile joins cleanly (no leaked thread, no use-after-free); worker panic does not re-panic in Drop
- [ ] #4 Eager materialize still works while the worker is busy (mutex serialization test)
- [ ] #5 No thread is spawned unless tier_up_async is called (lazy spawn test)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
BGT-2 landed 2026-07-06. JitBackend restructured to {shared: Arc<Shared>, worker: Mutex<Option<JoinHandle>>}. Shared owns the Mutex<Jit> (module+fbctx+slots), a bounded queue (Mutex<Queue{items:VecDeque,outstanding,shutdown}> + work_cv/idle_cv Condvars), done: Mutex<Vec<TierUpFinished>>, ready: AtomicUsize probe. Worker loop: recv->compile under inner mutex->push done->bump ready->dec outstanding->idle notify. Lazy spawn on first tier_up_async; Drop sets shutdown, notifies, joins (poison-safe via into_inner, never re-panics). Backend::tier_up_async (Queued at <64 depth / Busy full / Unsupported if shutting down) + tier_up_finished (ready-gated drain). tier_up_handle()->TierUpHandle::wait_idle() determinism lever. compile/compile_region/compile_with moved to impl Shared (offsets+caps there). 6 crate-local tests (all ACs). std threading only, no new deps. DoD: nextest --features unicorn 273/273 green minus fuzz; clippy clean; fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
