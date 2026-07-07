---
id: TASK-111
title: 'go-caddy P4 — netpoller: nonblocking sockets + epoll'
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
  - 'crate:tests'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. P4 netpoller: Fd::Epoll/Event (real host fds) + host_io_fd helper; epoll_create1/ctl/pwait + eventfd2; SyscallOutcome::EpollWait + driver chunked host-wait loop (futex_wait shape, exited backstop, zero-timeout inline); per-entry epoll_event marshaling (12B packed, aarch64-portable); fcntl F_GETFL/F_SETFL host-forward (O_NONBLOCK-masked). Acceptance go_net.rs: static Go stdlib-net TCP server serves one real HTTP response over real TCP, three ways (native/interp/JIT). Unblocked by task-132 (RCR + fault teardown). getsockname/accept4/read/write errno already faithful. Scope: task-133 (epoll on synthetic fds). Full suite 261/261.
<!-- SECTION:NOTES:END -->
