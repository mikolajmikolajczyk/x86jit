---
id: TASK-186
title: >-
  NativeOracle: real x86-host oracle for the fuzzer/differential (catches
  shared-semantics bugs where Unicorn cant decode VEX/EVEX)
status: To Do
assignee: []
created_date: '2026-07-09 12:51'
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
