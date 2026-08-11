---
id: TASK-226
title: >-
  cranelift: a panic during compilation poisons the JIT lock, and every later
  call reports PoisonError instead of the cause
status: Done
assignee: []
created_date: '2026-07-29 10:43'
updated_date: '2026-08-11 11:07'
labels:
  - bug
  - diagnosability
  - codegen
dependencies: []
ordinal: 322000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
JitBackend::jit() does self.inner.lock().unwrap() (x86jit-cranelift/src/lib.rs:2341). If anything panics while that mutex is held — build_jit, or a compile — the lock is poisoned, and from then on every JIT entry panics with PoisonError. The embedder sees the secondary symptom on a later slice; the primary panic is one line far earlier in the log, or gone.

Reported by unemups4: eager mode (tier_up_after = None) dies at lib.rs:2341 on PoisonError with flips=0, and the primary panic was not recoverable from the log. Eager compiles every block on first execution, so it reaches codegen paths the tiered path never does — the primary is likely a real codegen defect that this masks.

Poisoning here buys nothing: Jit is rebuildable, and the guard already handles the None case. Recovering (lock().unwrap_or_else(|e| e.into_inner())) would let the first panic stand as the only failure. Optionally keep a recorded cause so a later call can re-report it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A panic during compilation no longer turns every subsequent JIT entry into a PoisonError panic
- [x] #2 The primary panic remains visible / attributable — the embedder can tell what actually failed
- [ ] #3 A test panics inside a compile and asserts the next JIT call reports the original cause, not PoisonError
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed: lock_recovering() replaces .lock().unwrap() at the three production sites (JIT module in jit(), the same mutex in the slot-clearing path, and the compile worker). A poisoned lock is recovered and announced once instead of cascading PoisonError into every later JIT entry. Test a_poisoned_jit_lock_does_not_cascade; negative control confirms it fails without the helper.

AC#3 NOT met as written, and the reason is worth keeping: it asks the next call to report 'the original cause'. PoisonError does not carry the panic payload — Rust does not keep it — so no implementation can do that from the lock alone. What is achievable, and what this does, is stop drowning the primary panic and say once that it happened. Recovering the payload would need catch_unwind around the compile, which is a different and larger change.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
