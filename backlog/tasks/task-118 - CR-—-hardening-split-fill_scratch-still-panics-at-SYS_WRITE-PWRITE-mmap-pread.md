---
id: TASK-118
title: 'CR — hardening split: fill_scratch still panics at SYS_WRITE/PWRITE/mmap/pread'
status: To Do
assignee: []
created_date: '2026-07-06 11:10'
labels: []
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
