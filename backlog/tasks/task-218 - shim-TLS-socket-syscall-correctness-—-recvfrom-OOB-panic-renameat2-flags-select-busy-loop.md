---
id: TASK-218
title: >-
  shim: TLS socket-syscall correctness — recvfrom OOB panic, renameat2 flags,
  select busy-loop
status: Done
assignee: []
created_date: '2026-07-12 07:16'
updated_date: '2026-07-12 07:35'
labels:
  - 'crate:linux'
  - bug
  - code-review
dependencies: []
ordinal: 247000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Code-review findings on the task-215 TLS syscall work (all in x86jit-linux/src/shim.rs). Fix three: (1) SYS_RECVFROM slices &data[..n] where data=vec![0u8;len] but n comes from the kernel — a datagram read with MSG_TRUNC (or any n>len) panics the emulator with an out-of-bounds slice; clamp the copy to n.min(len) while still returning the true n. (2) SYS_RENAMEAT2 ignores its flags arg (R8): RENAME_NOREPLACE is silently turned into an overwrite and RENAME_EXCHANGE into a one-way move; honor the flags (map to libc renameat2, or at minimum return -EINVAL on unknown/unsupported flags instead of clobbering). (3) select/pselect6 marks every non-host fd 'always ready', so a mixed fd_set (host socket + pipe/regular file) collapses the host select to a zero-timeout poll and busy-loops at 100% CPU instead of blocking; make a mixed set still block on the host fds with the guest timeout and merge the always-ready non-host fds into the result.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 recvfrom with n>len (MSG_TRUNC/datagram) does not panic; copies min(n,len) and returns true n
- [ ] #2 renameat2 honors RENAME_NOREPLACE/RENAME_EXCHANGE (or rejects unknown flags) — no silent clobber
- [ ] #3 a select/pselect6 over a mixed host+non-host fd_set blocks on the host fds instead of busy-looping
- [ ] #4 cargo nextest (--features unicorn, minus fuzz_robustness) green; clippy -D warnings + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
