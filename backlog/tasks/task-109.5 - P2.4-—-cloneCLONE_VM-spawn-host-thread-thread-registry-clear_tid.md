---
id: TASK-109.5
title: P2.4 — clone(CLONE_VM) -> spawn host thread + thread registry + clear_tid
status: Done
assignee: []
created_date: '2026-07-06 11:09'
updated_date: '2026-07-06 13:05'
labels: []
milestone: go-caddy
dependencies: []
parent_task_id: TASK-109
ordinal: 114000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
clone arm builds child CpuState (RAX=0, RSP, CLONE_SETTLS->FsBase, PARENT/CHILD_SETTID writes, CHILD_CLEARTID recorded) and yields ThreadOp::Spawn; the driver (owns all three Arcs) spawns the vcpu loop. On thread exit: write 0 to clear_tid + futex_wake.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-07-06. clone(CLONE_VM) intercepted in handle_mt::clone_thread — builds child CpuState (RAX=0, RSP, CLONE_SETTLS->fs_base), PARENT/CHILD_SETTID writes via &vm, parent Rax=child_tid, next_tid moved onto LinuxShim (seeded pid+1, race-free under shim lock). Yields SyscallOutcome::Spawn{Box<CpuState>,child_tid,clear_tid}; driver spawn_thread does new_vcpu+assign+spawn over the 3 Arcs, pushes JoinHandle. clear_tid handshake (write 0 + futex_wake) in run_vcpu epilogue. Validated by P2.7.
<!-- SECTION:NOTES:END -->
