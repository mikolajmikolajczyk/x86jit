---
id: TASK-331
title: A write barrier that costs nothing in the steady state (host page protection)
status: To Do
assignee: []
created_date: '2026-08-13 13:44'
labels:
  - m6-smc
dependencies: []
priority: low
ordinal: 367000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-329 gave compiled stores an SMC write barrier: an inline range test against a code watermark, on every store. It is correct and it is not free — MEASURED at ~+9 hot instructions per store (10.3 -> 19.5 marginal, `store_write_barrier_stays_within_its_measured_budget`) and ~10% wall-clock on the store-heavy `memcpy` workload, with every other bench workload inside its noise band. The maintainer accepted that cost deliberately rather than trade correctness for it.

The reason it cannot be made cheap the way the watch barrier was: the watch probe is gated on `watch_count != 0`, which is zero for almost every guest, so it sinks into a cold block that is never entered. Code pages exist as soon as anything has run, so the SMC test has no such gate and is evaluated on every store by necessity. Squeezing the emitted sequence is already done — two loads (pointer + live value), `shr`, `movl`, `sub`, `cmp`, `jb`, plus two register copies the allocator inserts. Splitting the two gates into separate branches and replacing a constant-pool `and` with a 32-bit move took it from 22.9 to 19.5; there is little left.

The approach that removes the cost rather than shaving it is **host page protection** — map guest code pages read-only in the host mapping and let the store fault. Box64, FEX and QEMU-user all do this. Steady-state cost is zero.

What blocks it here, and why this is a task rather than a patch: the fault handler is a SIGNAL handler, and signal handling belongs to the embedder, not to `x86jit-core` (whose dependency set is exactly `{iced-x86}`, enforced by `x86jit-tests/tests/boundary.rs`). So the core would have to expose a hook and a contract — which pages it wants protected, what the embedder must call on a fault — and that contract is the actual design work. `unemulinux` already has a `sigsegv.rs`, so there is a consumer to design against.

Worth a `backlog decision` rather than a silent implementation: it changes the embedder contract.

Second, smaller idea if the full design is not worth it: bake the code range into each block as a compile-time IMMEDIATE (`sub` + `cmp` against constants, ~3 instructions instead of ~9), and invalidate compiled blocks when `mark_code` widens the range past what they baked. Cheaper emitted code, but a whole-cache flush per newly-touched code page — which may be worse during startup. Measure before choosing.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A decision record states which approach was chosen and why, including what it costs the embedder
- [ ] #2 If host page protection is chosen: the core exposes the hook without gaining a dependency, and boundary.rs still passes
- [ ] #3 The store barrier's marginal hot instruction count is re-measured and the budget test updated with the new number
- [ ] #4 The guest-self-patch tests in smc.rs still pass on both backends unchanged — the barrier's mechanism may change, its behaviour may not
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
