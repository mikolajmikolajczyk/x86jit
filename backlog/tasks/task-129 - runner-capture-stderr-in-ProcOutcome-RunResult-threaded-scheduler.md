---
id: TASK-129
title: 'runner: capture stderr in ProcOutcome / RunResult (threaded + scheduler)'
status: Done
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-12 18:46'
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

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 runner test: a guest writing to stderr has it captured in ProcOutcome/RunResult (threaded + scheduler paths both asserted)
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged c2ae410). Plumbing already existed (ProcOutcome.stderr surfaced by deferred scheduler + threaded driver, thread.rs:480). Added the missing AC test: drive_full returns the full ProcOutcome; stderr_deferred_program (main writes fd2, no clone -> scheduler path) + stderr_thread_program (sibling thread writes fd2 post-escalation -> threaded path); assert byte lands in ProcOutcome.stderr on interp+jit. Non-vacuous (threaded byte from a real host thread). 4 tests, clippy+fmt clean.
<!-- SECTION:NOTES:END -->
