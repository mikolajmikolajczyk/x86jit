---
id: TASK-149
title: GP-2 — SIGSEGV handler + guarded_run (feature lands)
status: To Do
assignee: []
created_date: '2026-07-07 11:02'
labels:
  - go-caddy
  - 'crate:linux'
  - 'goal:harden'
dependencies: []
ordinal: 158000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
doc-30 GP-2. sigsegv.rs: install-once SA_SIGINFO save+chain, TLS GuardSlot, classification (active + si_addr in span + PC in JIT code else re-raise), mcontext seam x86+aarch64, sigsetjmp in guarded_run + siglongjmp, hardware access kind. thread.rs/proc.rs -> guarded_run; x86jit-run Go -> reserve_guarded. Flip pin (addr+access). Invariant: no Drop local across call_block. Tests: write/nil-page/threaded/subprocess-honesty.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
