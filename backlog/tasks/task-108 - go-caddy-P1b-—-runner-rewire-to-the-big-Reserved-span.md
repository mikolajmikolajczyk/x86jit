---
id: TASK-108
title: go-caddy P1b — runner rewire to the big Reserved span
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-06 13:52'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. has_go_build_note (x86jit-elf) scans PT_NOTE for owner Go (survives strip; goblin keeps the NUL pad, trimmed). Runner (x86jit-run) keys off it: Go note -> Reserved 1 TiB NORESERVE span + run_threaded, structurally coupled (threaded process cant fork/exec per P2.8, so never hits the Reserved-fork panic memory.rs:296 nor the deferred scheduler); everything else stays Flat + Scheduler unchanged. Go layout: low stack (2 GiB top) + brk above image, high mmap arena [4 GiB, 516 GiB), all sparse. #14 invariants get a Reserved variant. Guest::reserved(span) added to the test harness (host_ram + one sparse region [heap_base, mmap_limit)). AC#2 proven by scout (Go boots past mallocinit + 768 GiB arena hint into minit); the three-way run pins in P3. Fixture hello_go.elf + .go committed. Note-detection test green.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
