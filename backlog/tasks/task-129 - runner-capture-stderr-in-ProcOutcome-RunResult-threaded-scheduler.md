---
id: TASK-129
title: 'runner: capture stderr in ProcOutcome / RunResult (threaded + scheduler)'
status: To Do
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-07 10:08'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 138000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope; do EARLY in caddy phase (debugging multiplier). LinuxShim captures stderr (shim.rs:511) and Guest::run_full returns it, but the threaded ProcOutcome (proc.rs:62) drops it. Go println/panics/runtime throws ALL land on stderr — need it visible the moment a bigger Go program breaks. Thread stderr through ProcOutcome + RunResult + the scheduler path.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
