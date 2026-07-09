---
id: TASK-164
title: >-
  lifter: non-temporal stores (movntdq/movnti/movntps/movntpd) unlifted ->
  UnknownInstruction
status: To Do
assignee: []
created_date: '2026-07-07 20:27'
updated_date: '2026-07-09 15:10'
labels:
  - 'crate:core'
  - go-caddy
  - 'goal:fix'
dependencies: []
ordinal: 173000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Go's memclr/memmove use non-temporal stores (movntdq, movnti, ...) for large blocks (caddy's .text has 12 movntdq + 3 sfence). None are in lift.rs — they fall through to UnknownInstruction. Cold in 'caddy version' (the large-clear path isn't hit), so not the task-161 corruption, but any bigger Go workload that zeroes/copies large spans will trap. Lower to a normal vector store (semantically identical in our coherent single-buffer model; the non-temporal cache hint has no architectural effect here) — reuse lift_vmov like movdqu (lift.rs:531). sfence/lfence/mfence are already no-ops (lift.rs:486), correct for our model. Add movnti (GPR non-temporal) similarly. Discovered during task-161 go-caddy investigation.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 movntdq/movntps/movntpd lower to a 16-byte vector store (as movdqu)
- [ ] #2 movnti lowers to a sized GPR store
- [ ] #3 differential test: movntdq to memory matches unicorn
- [ ] #4 existing ACs stand; additionally fuzzer/differential treats movnt* as plain stores (jit==interp on the written bytes)
<!-- AC:END -->



## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
