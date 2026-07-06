---
id: TASK-125
title: 'mt: blocking fd I/O (read/accept/epoll) as yielded outcomes'
status: To Do
assignee: []
created_date: '2026-07-06 12:51'
labels: []
milestone: go-caddy
dependencies: []
ordinal: 134000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope. A guest thread blocking in read()/accept()/epoll must not hold the shim lock; needs the same yield-by-value treatment as futex (SyscallOutcome::Blocking*). Phase-3 territory (Go/caddy netpoller); P2.x must not touch it.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
