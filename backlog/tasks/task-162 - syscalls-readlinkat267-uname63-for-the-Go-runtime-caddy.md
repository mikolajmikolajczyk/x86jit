---
id: TASK-162
title: 'syscalls: readlinkat(267) + uname(63) for the Go runtime / caddy'
status: Done
assignee: []
created_date: '2026-07-07 17:19'
updated_date: '2026-07-08 13:17'
labels:
  - go-caddy
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies:
  - TASK-153
ordinal: 171000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real caddy (task-153) calls readlinkat(267) (os.Executable: readlink /proc/self/exe) and uname(63) at startup; both hit the shim's -ENOSYS default. Non-fatal today (caddy continued past them to the GC crash, task after this), but for completeness + correct os.Executable/uname behavior: implement uname (fill a plausible utsname: sysname=Linux, release, machine=x86_64) and readlinkat for /proc/self/exe (return the entrypoint path). Low risk, mechanical.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 uname(63) fills a valid utsname; readlinkat(267) of /proc/self/exe returns the entrypoint path; Go's os.Executable/runtime don't proceed with garbage
<!-- AC:END -->



## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
shim.rs: add SYS_UNAME=63 (write struct utsname to the guest buffer) and SYS_READLINKAT=267 (handle /proc/self/exe -> argv[0]/entrypoint path; ENOSYS/EINVAL for others). Mirror existing syscall arms. Refs: the gap log lines 'unhandled syscall 267/63 -> -ENOSYS' when running caddy.
<!-- SECTION:PLAN:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
