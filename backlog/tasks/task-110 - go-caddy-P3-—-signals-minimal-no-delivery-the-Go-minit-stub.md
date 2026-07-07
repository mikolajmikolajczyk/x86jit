---
id: TASK-110
title: go-caddy P3 — signals-minimal (no delivery; the Go minit stub)
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
  - 'crate:tests'
milestone: go-caddy
dependencies: []
ordinal: 119000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
sigaltstack / rt_sigprocmask / rt_sigaction stubs Go's minit needs (a 2-line stub is a P2->DoD-2 dependency; sigaltstack -ENOSYS is fatal for Go). No real delivery. Also the guard-page path for the JIT/interp unmapped-in-span decision would live here.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 static Go hello world prints hello three ways (native/interp/JIT)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. Signal/advice stubs Go minit needs, no delivery. madvise(28)->0 (advisory; Go doesnt rely on advice-zeroing). sigaltstack(131) + rt_sigprocmask(14): real per-thread bookkeeping via ThreadCtx{altstack,sigmask} + handle_mt intercepts (Go installs one alt stack + mask per M); single-threaded path uses shim fields; shared do_sigaltstack/do_sigprocmask helpers (write old back so a query reads SS_DISABLE not garbage; ss_size<MINSIGSTKSZ -> -ENOMEM). rt_sigaction(13): process-wide [u8;32]*64 table in shim (Go initsig queries every signal for fwdSig; write old back). prlimit64(302): branch on resource (was ignored -> 8MiB for everything); RLIMIT_STACK{8MiB,inf}, RLIMIT_NOFILE{1024,4096}. getrandom/sched_getaffinity already correct (kept; getrandom deterministic 0x42 is load-bearing, real entropy = task-128). Acceptance: go_hello.rs three-way (native/interp/JIT) static Go hello over Reserved span + run_threaded -> hello from go stdout. Full suite 205/205.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
