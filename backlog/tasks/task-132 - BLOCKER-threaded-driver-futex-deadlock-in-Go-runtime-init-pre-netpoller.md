---
id: TASK-132
title: 'BLOCKER: threaded-driver futex deadlock in Go runtime init (pre-netpoller)'
status: Done
assignee: []
created_date: '2026-07-06 14:46'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
  - 'crate:cranelift'
  - 'crate:linux'
milestone: go-caddy
dependencies: []
ordinal: 141000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A static Go *net* binary deadlocks during runtime init, BEFORE reaching net.Listen/the netpoller. Diagnosis (interp): main (tid 1000) and one worker M (tid 1002) both park in FUTEX_WAIT on a two-address ping-pong handshake (heap addr 0x2494xxxxx <-> bss addr 0x5ee538); a wake is lost so both sleep forever, while sysmon (tid 1001) spins clock_gettime+nanosleep without ever issuing a futex_wake. Only ops 0/1 (WAIT/WAKE) are used — no bitset ops, no unmodeled-futex gap. Not a missing syscall, not a dead worker (no worker Err), not the P2.6 real-sleep (capping sleep to 1ms did not unblock). pthreads.elf (mt_shim, 4 threads + mutex + join) PASSES with the same futex code, so it is Go-runtime-handshake-specific. INDEPENDENT of P4 epoll (never reached). Blocks go_net.rs (P4 acceptance, currently #[ignore]d) and all of go-caddy P4/P5. Likely a lost-wakeup edge in the condvar+generation futex (thread.rs futex_wait/futex_wake) OR a memory-visibility issue on the futex word across host threads. Next: minimal C reproducer of the note/mutex ping-pong; deeper futex logging with word values + happens-before; consider Fable consult on Go futex semantics vs our impl.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed 2026-07-06 (Fable-5 diagnosis, confirmed via gdb host stacks). Two-part fix: (1) x86jit-core: RCR/RCL implemented across IR (IrOp::Rcl/Rcr, consume CF like Adc), lifter (D1/C1/D3 group-2 /2 /3), interp (bit-serial rcl/rcr, count mod width+1), JIT (emit_rcx, bounded Cranelift loop). Validated interp==JIT==Unicorn: rotate_through_carry_by_one/widths_and_counts + div_by_constant_carry_fold (the exact Go div-by-7 magic-multiply+add+rcr+shr pattern). (2) x86jit-linux/thread.rs: fault_teardown() on run_vcpu Err paths sets exited+notify_all so a faulting thread unparks siblings and run_threaded surfaces the error instead of hanging on the worker join. Pinned by fault_teardown_releases_indefinite_waiter. The futex model was NEVER broken — the deadlock was a swallowed main-thread RCR trap masked by the join. Full suite 261/261.
<!-- SECTION:NOTES:END -->
