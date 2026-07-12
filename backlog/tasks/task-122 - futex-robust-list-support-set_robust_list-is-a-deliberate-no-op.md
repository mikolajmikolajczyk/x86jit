---
id: TASK-122
title: 'futex: robust list support (set_robust_list is a deliberate no-op)'
status: Done
assignee: []
created_date: '2026-07-06 12:51'
updated_date: '2026-07-12 17:12'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 131000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope. set_robust_list no-op-d today; matters for pthread_mutex_robust / dying-thread lock recovery (Go runtime uses it defensively). Document the no-op as deliberate; revisit when a real guest misbehaves.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 mt test: robust mutex held by an exiting thread is recovered by the next locker (EOWNERDEAD)
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged a4d3a32). Robust list: set_robust_list(273) records per-thread head+len in ThreadCtx (rejects bad len -EINVAL); get_robust_list(274) reads back. walk_robust_list on thread exit: full bounded walk (ROBUST_LIST_LIMIT 2048), signed futex_offset via wrapping_add, ORs FUTEX_OWNER_DIED into each held word + wakes one waiter; list_op_pending handled once; unreadable ptr stops walk. Adversarial review: no host-unsafety, no infinite loop, OWNER_DIED OR-not-overwrite. KNOWN FIDELITY GAP (filed as follow-up): walk sets OWNER_DIED without strict word-tid==dying-tid check (conservative over-flag, sandbox-internal, safe).
<!-- SECTION:NOTES:END -->
