---
id: TASK-133
title: epoll_ctl on synthetic fds (shim pipes/files/stdio) -> pollable
status: To Do
assignee: []
created_date: '2026-07-06 14:47'
updated_date: '2026-07-07 10:07'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 142000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 P4 scope. epoll_ctl targeting a non-host fd (shim pipe, Fd::File, stdio) currently returns -EPERM (host_io_fd is None) — the kernels honest answer for a non-pollable regular file, one-shot gap. Making shim pipes pollable (host pipe2 backing or eventfd shadowing) is its own task; caddy may need stdin polling. Do not build speculatively.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
