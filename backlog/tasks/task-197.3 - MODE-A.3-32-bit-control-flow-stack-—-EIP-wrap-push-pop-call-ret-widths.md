---
id: TASK-197.3
title: 'MODE-A.3: 32-bit control flow + stack — EIP wrap, push/pop/call/ret widths'
status: To Do
assignee: []
created_date: '2026-07-10 10:32'
updated_date: '2026-07-10 10:43'
labels:
  - guest-modes
dependencies:
  - TASK-197.1
parent_task_id: TASK-197
ordinal: 224000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Branch targets, call/ret return addresses and the dispatcher PC truncate to 32 bits in Compat32. Stack ops honour 32-bit default operand size (66h flips to 16-bit push/pop), ESP wraps at 2^32. Writing a 32-bit reg in Compat32 keeps storing zero-extended into the u64 backing state (no architectural upper bits — harmless, but pin with a test so JIT and interp agree).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 call/ret/jcc/jmp round-trip with 32-bit truncated targets (unicorn-diffed)
- [ ] #2 interp == JIT on a 32-bit control-flow + stack differential batch
- [ ] #3 push/pop/call frames are 4-byte (2-byte under 66h); ESP wraps mod 2^32 (unicorn-diffed + interp==JIT cases)
<!-- AC:END -->





## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
