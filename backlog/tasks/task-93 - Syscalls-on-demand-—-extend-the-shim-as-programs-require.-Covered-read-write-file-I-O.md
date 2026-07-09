---
id: TASK-93
title: >-
  Syscalls on demand — extend the shim as programs require. **Covered:**
  read/write file I/O
status: To Do
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-09 15:10'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: open-backlog
dependencies: []
ordinal: 93000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
extend the shim as programs require. **Covered:** read/write file I/O incl. **writable passthrough** (`pwrite`/`ftruncate`/`fsync`/`unlink`) + `stdin`, `mmap`/`brk`, `stat`/`lstat`/`fstat`, `writev`, `lseek`, `fcntl`, `access`, `clock_gettime`/`gettimeofday`, chmod/chown no-ops, sig/uid/pid stubs, `clone`/`futex` (threaded guests). `dup`/`dup2` (busybox gzip dups its input onto fd 0), `readv` (libjpeg). **Next:** `getrandom`, `mprotect` (beyond no-op), sockets (`msghdr`), `pipe`. (testing.md §12.5)
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 every newly covered syscall lands with a shim-level or whole-program test asserting its observable behavior (no syscall added test-free)
<!-- AC:END -->
