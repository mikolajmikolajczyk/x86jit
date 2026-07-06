---
id: TASK-139
title: 'BGT-5 — surface completion: bench (inline vs background), runner knob, docs'
status: To Do
assignee: []
created_date: '2026-07-06 18:23'
updated_date: '2026-07-06 18:47'
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
Decision #4 framing (maintainer): x86jit-run today = SYNC tier-up @50 for JIT engine (lib.rs:275, TIER_UP_AFTER=50). BGT-5 decides whether to FLIP the runner's JIT default sync->background. Gate the flip on: BGT-4 hardening green AND background >= sync on the runner corpus with zero correctness regressions. If background not clearly better/stable, keep sync as runner default (still far better than eager) and leave background opt-in. Bench should sweep the threshold (50) too. Keep eager/inline/background all selectable via flag/env for debugging; differential corpus stays off-tier regardless. D2 recorded as decision-5 (accepted). #3 confirmed: Busy=stay-interpreted, queue cap 64.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
