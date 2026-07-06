---
id: TASK-143
title: 'VCLK-3 — acceptance: eager-JIT go_http leg + load de-flake evidence'
status: To Do
assignee: []
created_date: '2026-07-06 20:06'
labels:
  - go-caddy
dependencies:
  - TASK-142
ordinal: 152000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-134 DoD (threaded-clock-plan.md VCLK-3). Add go_http_serves_index_jit_eager to x86jit-tests/tests/go_http.rs — JitBackend with NO .tier_up dodge, the case that races today — and update the task-134 comment block (go_http.rs:109-117). Audit go_net.rs's JIT leg the same way. Keep the tiered leg (exercises FD-TIER) unless the maintainer resolves open decision 3 otherwise. De-flake evidence: run the interp legs under synthetic host load (e.g. stress-ng --cpu $(nproc) alongside) before/after and record results in task notes — documented manual verification, not a CI assertion. Recommended: a threaded micro-guest pinning termination of a 'while (now < start+30ms) n++' loop on both backends (termination-shaped, respects the non-assertion rule) — open decision 5. Tune MT_CLOCK_TICK_NS here if the eager leg demands it (open decision 2).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 go_http_serves_index_jit_eager passes on x86 (eager JIT, no tier-up)
- [ ] #2 Loaded-host interp runs recorded in task notes: flaky before, stable after
- [ ] #3 No new test asserts threaded wall-clock values (non-assertion rule)
- [ ] #4 ARM leg verified via the manual CI workflow
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
