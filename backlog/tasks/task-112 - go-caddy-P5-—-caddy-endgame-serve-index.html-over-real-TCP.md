---
id: TASK-112
title: 'go-caddy P5 — caddy endgame: serve index.html over real TCP'
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
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
