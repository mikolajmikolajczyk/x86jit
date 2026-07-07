---
id: TASK-128
title: >-
  shim: getrandom entropy mode (Deterministic | HostEntropy) — mandatory before
  TLS
status: To Do
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
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
