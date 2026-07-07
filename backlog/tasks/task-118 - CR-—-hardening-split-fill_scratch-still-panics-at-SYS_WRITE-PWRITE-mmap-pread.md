---
id: TASK-118
title: 'CR — hardening split: fill_scratch still panics at SYS_WRITE/PWRITE/mmap/pread'
status: Done
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-07-07 10:38'
labels:
  - 'crate:linux'
  - 'goal:harden'
milestone: code-review
dependencies: []
ordinal: 127000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The EFAULT hardening added try_fill_scratch/try_resize_scratch at read/bind/connect/setsockopt/writev, but the panicking fill_scratch (+ inline resize+write_bytes.expect) remain at SYS_WRITE(759)/PWRITE64(1180)/mmap(976)/pread(1044). Deeper fix: make fill_scratch itself fallible everywhere.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed: removed the panicking fill_scratch; SYS_WRITE + SYS_PWRITE64 now use try_fill_scratch (-EFAULT on unmapped/bogus source); mmap file-backed + MAP_FIXED and SYS_PREAD64 use try_resize_scratch (-ENOMEM on bogus length) with best-effort/fallible write_bytes (-EFAULT on unmapped dest) instead of .expect. No syscall arm can now abort the host on guest-controlled ptr/len. Tests green.
<!-- SECTION:NOTES:END -->
