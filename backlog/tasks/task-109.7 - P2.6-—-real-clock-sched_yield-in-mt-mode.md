---
id: TASK-109.7
title: P2.6 — real clock + sched_yield in mt mode
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-06 13:13'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 116000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Switch clock_gettime/nanosleep to host CLOCK_MONOTONIC + real sleep when threads exist (virtual tick clock keeps the single-threaded corpus deterministic; record the determinism-loss decision). sched_yield -> yield_now, 0.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. Clock anchor: LinuxShim.clock_anchor Option<(Instant,u64)> set at first clone (flip to threaded); now_ns() returns virtual tick when None else base_ns+anchor.elapsed() (real host monotonic, never jumps backward). tick_clock split over now_ns. nanosleep/clock_nanosleep intercepted in handle_mt when reached via driver -> sleep_mt yields SyscallOutcome::Sleep(Duration); driver sleeps interruptibly in FUTEX_POLL chunks checking exited. sched_yield(24) -> Rax=0 + SyscallOutcome::Yield -> yield_now. Single-threaded corpus unchanged (anchor None). decision-4 records determinism trade. Unit test clock_is_deterministic_until_threaded_then_anchors + full suite 201/201.
<!-- SECTION:NOTES:END -->
