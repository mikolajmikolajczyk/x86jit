---
id: TASK-108
title: go-caddy P1b — runner rewire to the big Reserved span
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
ordinal: 108000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Switch the OCI runner arena to a Reserved span via Vm::with_backend_host_ram + x86jit_linux::hostmem::reserve behind a per-image heuristic; observe Go's mallocinit abort move. Wants a Go fixture (and really P2 to reach the next wall).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Runner can back a guest VM with a Reserved NORESERVE span
- [ ] #2 Go's mallocinit abort observably moves past page-summary reservation
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
