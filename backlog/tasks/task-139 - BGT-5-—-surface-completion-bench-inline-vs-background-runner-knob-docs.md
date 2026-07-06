---
id: TASK-139
title: 'BGT-5 — surface completion: bench (inline vs background), runner knob, docs'
status: Done
assignee: []
created_date: '2026-07-06 18:23'
updated_date: '2026-07-06 20:05'
labels: []
milestone: m-0
dependencies:
  - TASK-137
  - TASK-138
ordinal: 148000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Phase 5 of background-tier-plan.md (doc-27). Measure and finish the surface; the numbers feed the maintainer's open decision on the x86jit-run default.

- x86jit-bench: background mode next to tier_from_env() (workloads.rs:97,275); compare eager vs inline-tier vs background-tier on sqlite/lua/go-startup — wall time, plus max single-dispatch stall if cheap to record (the latency spike this feature removes); record a perf snapshot alongside the existing ones.
- x86jit-run (lib.rs:275, TIER_UP_AFTER): env knob for background; default stays inline pending the numbers (open decision 4 in doc-27).
- Tune the queue capacity default (64) against the bench.
- Docs: backlog/docs/status.md + architecture.md updated; deferred.md notes what deliberately stayed out (compile pool pending FD-AOT B0.2, region tier-up = BGT-6, per-span epoch).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Bench numbers recorded (perf snapshot) comparing eager / inline tier-up / background tier-up on at least sqlite, lua, and one Go workload
- [ ] #2 x86jit-run has an env knob for background tier-up; default unchanged (inline)
- [ ] #3 status.md, architecture.md, deferred.md updated to reflect the delivered feature and its deliberate exclusions
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
BGT-5 landed 2026-07-06. Bench: X86JIT_BG_TIER knob in workloads.rs (bg_from_env at both run_guest sites) + go_startup() (hello_go over threaded driver + Reserved span). experiment() now compares eager/inline=50/bg=50 across fib/sha/sqlite/lua/go-startup. NUMBERS (one host, min of 3): sqlite 1209ms->100ms(12x)->32ms(37.9x), lua 510->98(5.2x)->29(17.4x), go-startup 771->64(12x)->24ms(31.7x), sha256 19->14->12, fib32 flat. Background 2.6-3.8x faster than inline on startup-heavy (compile overlaps interpretation). x86jit-run: X86JIT_BG_TIER env knob, default inline (per AC#2; doc-27 #4 flip decision now data-backed toward background). Queue cap 64 kept (single-vcpu bench doesn't stress it). Docs: status.md (perf snapshot + delivered feature), architecture.md (core-driven tier-up + backend worker), deferred.md (compile pool pending FD-AOT B0.2, region tier-up=BGT-6, per-span epoch). DoD: nextest --features unicorn 281/281 green minus fuzz; clippy clean; fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
