---
id: TASK-227
title: >-
  126-followup: centralize clone classification + fix handle_mt vfork misroute +
  test gap
status: Done
assignee: []
created_date: '2026-07-12 15:44'
updated_date: '2026-07-12 16:08'
labels:
  - 'crate:linux'
  - 'goal:refactor'
dependencies: []
ordinal: 256000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-126 review debt. (1) Centralize clone classification: is_clone_vm (thread.rs:55, CLONE_VM|CLONE_THREAD) is canonical; shim.rs handle() SYS_CLONE arm (2137, CLONE_VM-only) and handle_mt (3205, CLONE_VM-only) duplicate the SYS_CLONE/CLONE_VM constants and logic. (2) CORRECTNESS: handle_mt:3205 routes any CLONE_VM to clone_thread — a vfork/posix_spawn (CLONE_VM|CLONE_VFORK, no CLONE_THREAD) reaching handle_mt after escalation is misrouted to thread-spawn instead of fork_eagain. Tighten to the canonical classifier. (3) TEST GAP (from 126 notes): forked-child-escalates path (proc.rs reap_pending Escalate arm) fixed but untested; add a direct test. Add a vfork-in-threaded-process -> EAGAIN regression test.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Single canonical clone classifier used by proc.rs peek, shim handle(), and handle_mt (no duplicated SYS_CLONE/CLONE_VM/CLONE_THREAD constants across the three)
- [x] #2 handle_mt routes only real thread clones (CLONE_VM|CLONE_THREAD) to clone_thread; vfork/posix_spawn (CLONE_VM|CLONE_VFORK, no CLONE_THREAD) -> fork_eagain; regression test proves it
- [x] #3 forked-child-escalates path has a direct test
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged e737fa2 via ea25f5f). Centralized clone classification into thread.rs::is_thread_clone(rax,rdi) — is_clone_vm delegates; handle_mt clone arm + handle gap-log source the canonical CLONE_VM/SYS_CLONE. CORRECTNESS FIX: handle_mt now routes to clone_thread ONLY for CLONE_VM|CLONE_THREAD; vfork/posix_spawn (CLONE_VM|CLONE_VFORK) falls through to fork_eagain. Tests: handle_mt_vfork_shaped_clone_forks_not_spawns (unit, fail-without-fix verified) + forked_child_escalates_and_is_reaped_{interp,jit} (closes the 126 test gap). Adversarial review: no bugs (gap-log broad-predicate reasoning sound; both tests non-vacuous). 617/617, clippy+fmt clean. clone3(435) still deferred (separate).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
