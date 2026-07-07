---
id: TASK-109.6
title: 'P2.5 — per-thread identity: gettid / set_tid_address / tgkill'
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 115000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real per-thread tid (not ==pid); set_tid_address stores clear_tid; tgkill routing. Per-thread ctx passed into handle_mt; registry in ThreadShared. exit(60) ends one thread, exit_group(231) ends the process (distinction rides in the returned ThreadOp).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. ThreadCtx{tid,clear_tid} owned per run_vcpu frame, passed &mut into handle_mt. Intercepts gettid->ctx.tid, set_tid_address->ctx.clear_tid, exit(60)->ThreadExit BEFORE delegating to handle() (single-process semantics untouched, differential corpus keeps its oracle). exit vs exit_group split: SyscallOutcome::ThreadExit vs ProcessExit; alive:AtomicU64 in ThreadShared (init 1, +1/spawn, -1/exit) publishes last-thread exit code (Linux: process lives until last thread). No HashMap registry (Fable-5 ruling a). Validated by P2.7.
<!-- SECTION:NOTES:END -->
