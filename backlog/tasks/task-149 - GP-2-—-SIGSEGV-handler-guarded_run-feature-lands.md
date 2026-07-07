---
id: TASK-149
title: GP-2 — SIGSEGV handler + guarded_run (feature lands)
status: Done
assignee: []
created_date: '2026-07-07 11:02'
updated_date: '2026-07-07 11:25'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
GP-2 landed — the feature is active. sigsegv.rs: install-once SA_SIGINFO (save+restore-on-non-guest-fault), TLS GuardSlot, glibc __sigsetjmp/siglongjmp (JmpBuf oversized), manual si_addr (SigfaultPrefix offset 16), mcontext seam (x86-64 gregs[REG_ERR]&2; aarch64 stub->Read), guarded_run(cpu,vm,budget) wraps Vcpu::run. Classification: active guard + si_addr in [base,base+size) -> longjmp -> Exit::UnmappedMemory; else restore old disposition + re-fire (honest crash). Wired thread.rs run_vcpu + proc.rs run_process -> guarded_run; x86jit-run Go path -> reserve_guarded. Tests (guard_pages.rs): guarded load/store/nil-deref fault interp+JIT; host_fault_outside_span_still_crashes (subprocess, signal 11). Regression: go_hello/go_net/go_http (incl eager JIT) + mt/mt_shim/pipe/threaded_driver all green under guards. RIP still stale (GP-3). glibc host assumption noted.
<!-- SECTION:NOTES:END -->
