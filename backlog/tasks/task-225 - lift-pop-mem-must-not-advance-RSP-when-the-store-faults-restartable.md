---
id: TASK-225
title: 'lift: pop [mem] must not advance RSP when the store faults (restartable)'
status: Done
assignee: []
created_date: '2026-07-12 10:38'
updated_date: '2026-07-12 12:35'
labels:
  - 'crate:core'
  - bug
  - code-review
dependencies: []
ordinal: 254000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Deferred from task-223 (Fable review). 'pop [mem]' reads [rsp], stores to [mem], then adds 8 to RSP — but the RSP commit currently happens even if the store to [mem] faults, leaving RSP already advanced. Hardware makes pop restartable: a faulting destination store must leave RSP unchanged (fault-before-commit). Fix in x86jit-core/src/lift/control.rs (lift_pop) — order the store before the RSP WriteReg, or otherwise ensure RSP is only committed after the store succeeds. Confirmed real by the 223 agent (interp: rsp=0x8008 after an UnmappedMemory{Write} fault). Verify interp==jit and add a faulting-pop-[mem] test. LOW severity (rare fault path).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 pop [mem] with a faulting destination store leaves RSP unchanged (interp==jit, matches hardware restartability)
- [ ] #2 cargo nextest (--features unicorn, minus fuzz_robustness) green; clippy -D warnings + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
