---
id: TASK-111
title: 'go-caddy P4 — netpoller: nonblocking sockets + epoll'
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
ordinal: 120000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Extend Phase-0 Fd::Socket with the Go runtime's epoll_create1/epoll_ctl/epoll_wait, nonblocking accept/read/write; release the shim lock while blocked in epoll_wait (reuse the P2 blocking discipline).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
