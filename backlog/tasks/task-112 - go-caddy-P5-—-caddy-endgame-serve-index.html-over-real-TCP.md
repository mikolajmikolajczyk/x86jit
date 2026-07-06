---
id: TASK-112
title: 'go-caddy P5 — caddy endgame: serve index.html over real TCP'
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-06 17:57'
labels: []
milestone: go-caddy
dependencies: []
ordinal: 121000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Run caddy (or a Go file-server) serving index.html, reachable from host curl, three ways. The endgame of the go-caddy roadmap.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
P5 COMPLETE three ways 2026-07-06. Go net/http file-server (httpserve_go.elf, http.FileServerFS) serves index.html over real host TCP: native + interp + TIERED JIT, all green (go_http.rs, 2 tests, 0 skipped). JIT leg uses FD-TIER tier_up(Some(50)) (task-106): Go's run-once startup/netpoller stays interpreted -> dodges the decision-4 host-anchored clock race -> serves in 4.0s; hot runtime loops still compile. Eager JIT alone fails (clock races, root-caused, task-134). DoD met: nextest --features unicorn green minus fuzz; clippy clean; fmt clean.
<!-- SECTION:NOTES:END -->
