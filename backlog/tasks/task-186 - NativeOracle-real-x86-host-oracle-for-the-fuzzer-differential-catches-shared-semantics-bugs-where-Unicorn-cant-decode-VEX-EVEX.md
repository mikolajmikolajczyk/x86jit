---
id: TASK-186
title: >-
  NativeOracle: real x86-host oracle for the fuzzer/differential (catches
  shared-semantics bugs where Unicorn cant decode VEX/EVEX)
status: Done
assignee: []
created_date: '2026-07-09 12:51'
updated_date: '2026-07-09 14:14'
labels:
  - code-review
dependencies: []
ordinal: 210000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The only automatic correctness net for vector ops is jit==interp — which catches JIT-vs-interp codegen divergence but NOT a shared-semantics bug (both interp and codegen written from the same wrong mental model ship green). Unicorn cannot be the oracle for VEX/EVEX (its QEMU build drops VEX.vvvv), so AVX/AVX2/AVX-512 (the biggest op surface, ~66 V-ops, and the roadmap 168.5.x) have NO independent oracle at all. Build a NativeOracle (see the deferred task-107): on an x86-64 host, execute the guest snippet on the REAL CPU (the hlt-trap / non-privileged approach) and compare full state — a true oracle that catches shared-semantics bugs for every op the host CPU supports, incl. VEX/EVEX. Wire it as an oracle leg in the fuzzer and differential harness (x86 hosts only; ARM keeps jit==interp+Unicorn). This is the highest-value missing net for the upcoming AVX-512 work. Depends conceptually on task-107.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (increment 1). Added x86jit-tests/src/native.rs: run_native(&VectorInput)->Option<RunOutcome>. Forks a child that loads guest GPRs/flags/XMM from a fixed low control page via an iced-assembled stub, jmps to entry, runs on the bare CPU; the terminating hlt (#GP→SIGSEGV, on a sigaltstack) is caught by an async-signal-safe handler that snapshots the register file from the ucontext (GPR+RIP+RFLAGS+XMM) into a MAP_SHARED page and _exits. Parent waitpids, reads it back + guest memory. Crash-isolated: unsupported insn (SIGILL)/non-hlt fault/timeout(alarm 2s)/sub-mmap_min_addr VA => None => caller skips. Wired as native_matches_interp in tests/fuzz.rs (299/299 fuzzer seeds ran natively and matched interp, incl. BMI ops Unicorn mis-decodes) + native::tests smoke test pinning the mechanism. Bumped fuzzer CODE/SCRATCH to 0x210000/0x220000 (above mmap_min_addr, clear of the 0x200000 control window). Full suite 362/362 green (--features unicorn), clippy+fmt clean. Follow-up: YMM/ZMM capture from the XSAVE area for when the fuzzer emits AVX (filed).
<!-- SECTION:NOTES:END -->
