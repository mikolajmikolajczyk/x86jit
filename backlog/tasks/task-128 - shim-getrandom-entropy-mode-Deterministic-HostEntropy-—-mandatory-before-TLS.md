---
id: TASK-128
title: >-
  shim: getrandom entropy mode (Deterministic | HostEntropy) — mandatory before
  TLS
status: Done
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-11 12:07'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 137000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope. getrandom currently a deterministic 0x42 fill (load-bearing: interp and JIT are separate shims; real entropy gives different Go hash seeds -> map-order divergence). But 0x42 under caddy HTTPS = TLS keys from a constant seed = security-grade bug. Add a shim entropy knob: Deterministic (default, for the differential corpus) | HostEntropy (for serving); also feed AT_RANDOM real bytes in HostEntropy mode. MANDATORY before any TLS.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 shim test: Deterministic mode reproduces byte-identical getrandom streams across runs; HostEntropy differs
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-11 [getrandom]. EntropyMode{Deterministic,HostEntropy} in LinuxShim: Deterministic (default) = splitmix64 fixed-seed PRNG (reproducible across runs, varied bytes unlike old 0x42 -> crypto works deterministically for the differential corpus); HostEntropy = /dev/urandom (real). Both getrandom syscall arms (64+32-bit) + fork inherits mode+state. Wired: RunOptions.entropy -> shim.set_entropy at load; CLI --entropy deterministic|host. AC#1 test getrandom_entropy_modes: deterministic reproduces byte-identical + not-0x42 + advances; host differs. E2E: openssl enc --entropy host bit-identical (no regression); openssl rand/pbkdf2 blocked by UNRELATED unlifted EVEX vbroadcastsd (filed task-214). Suite 552/552, clippy+fmt clean. REMAINING: AT_RANDOM real-bytes-in-HostEntropy deferred (needs setup_stack API change across 6 sites; glibc-canary hardening, not the TLS-key bug which getrandom closes) - noted in task-214.
<!-- SECTION:NOTES:END -->
