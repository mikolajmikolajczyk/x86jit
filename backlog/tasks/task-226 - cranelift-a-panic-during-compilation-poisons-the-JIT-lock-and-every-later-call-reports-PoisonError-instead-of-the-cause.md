---
id: TASK-226
title: >-
  cranelift: a panic during compilation poisons the JIT lock, and every later
  call reports PoisonError instead of the cause
status: To Do
assignee: []
created_date: '2026-07-29 10:43'
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
- [ ] #1 A panic during compilation no longer turns every subsequent JIT entry into a PoisonError panic
- [ ] #2 The primary panic remains visible / attributable — the embedder can tell what actually failed
- [ ] #3 A test panics inside a compile and asserts the next JIT call reports the original cause, not PoisonError
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
