---
id: TASK-109.8
title: 'P2.7 — pthreads.elf through the production shim + driver, both engines (DoD-1)'
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-06 13:05'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 117000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Wire the acceptance program through the real shim (not the mt.rs toy handle). Replace/augment the mt.rs test with a shim-driven one. Closes M7-T5 properly.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. x86jit-tests/tests/mt_shim.rs: pthreads.elf (4 threads x100k under futex mutex) through PRODUCTION run_threaded, both engines, vs native reference -> 400000. Proves P2.4 clone-spawn + P2.5 identity/exit end-to-end. mt.rs toy handle retained as the lower-level reference. ARM/weak-host ordering explicitly out of scope (M7-T4).
<!-- SECTION:NOTES:END -->
