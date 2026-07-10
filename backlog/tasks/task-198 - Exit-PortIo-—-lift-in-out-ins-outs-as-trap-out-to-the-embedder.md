---
id: TASK-198
title: 'Exit::PortIo — lift in/out/ins/outs as trap-out to the embedder'
status: To Do
assignee: []
created_date: '2026-07-10 10:33'
updated_date: '2026-07-10 10:43'
labels:
  - guest-modes
  - machine-exit
dependencies: []
priority: medium
ordinal: 227000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
First piece of the machine Exit surface: port I/O instructions (`in`, `out`, `ins`, `outs`, incl. rep forms) lift to a new `Exit::PortIo { port, size, direction, .. }` instead of Unsupported. The embedder answers reads by writing EAX/AL and resuming — same trap-out shape as MMIO/syscall. Independent of guest modes (works in Long64 today); prerequisite for any machine-style embedder (DOSBox-class, firmware). Cheap and self-contained.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 ins/outs (+rep) either exit per-element or are documented-rejected — decided and tested
- [ ] #2 in/out (imm8 and DX forms, sizes 1/2/4) exit with port, size, direction; guest resumes with the embedder-provided value — round-trip integration test with scripted embedder answers, interp and JIT
- [ ] #3 A test (mmio_device-style, may double as example) exercises an end-to-end port read/write round-trip
<!-- AC:END -->







## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
