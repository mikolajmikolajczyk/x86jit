---
id: decision-4
title: 'Threaded processes trade the deterministic virtual clock for anchored host monotonic time at first clone'
date: '2026-07-06 12:30'
status: superseded
---

> **Superseded by [[decision-6]]** (2026-07-07), on the **clock value domain**: mt
> mode now reads a rate-controlled *virtual* monotonic value, not host-anchored real
> time. Three clauses of this decision **carry forward unchanged** into decision-6:
> real host blocking (`nanosleep`/`futex`/`epoll` still really wait), single-threaded
> virtual-clock preservation, and the threaded-timing non-assertion rule.

**Deciders:** Mikołaj Mikołajczyk (architect consult: Fable 5)

## Context

Single-threaded guests read time from a **virtual tick clock** (`LinuxShim::clock_ns`):
every `clock_gettime`/`gettimeofday` read advances a fixed 1 ms quantum and `nanosleep`
advances it by the requested amount. Time is therefore a pure function of the syscall
sequence — fully deterministic, which is what lets the differential
`native == interp == JIT` corpus assert on real programs, and what keeps a
sleep-until-deadline loop from spinning forever (#13).

That model breaks down the moment a process has **real sibling threads** (go-caddy P2,
`clone(CLONE_VM)`):

- A `nanosleep` that only advances a virtual counter does not actually block, so a
  worker polling a deadline burns a host CPU instead of yielding it — and a real
  producer/consumer handoff between threads has no wall-clock to synchronize against.
- Multiple threads reading the virtual clock concurrently would see a value that
  depends on interleaving, so it is no longer deterministic *anyway*.

## Decision

**At the first accepted `clone(CLONE_VM)`, flip the process to "mt mode" and anchor the
clock to real host `CLOCK_MONOTONIC`.** The anchor records
`(Instant::now(), clock_ns)` at the flip; thereafter monotonic time is
`clock_ns + anchor.elapsed()`. This is genuine host time yet **never jumps backward**
across the switch (the virtual clock may sit ahead of or behind boot time). In mt mode,
`nanosleep`/`clock_nanosleep` perform a real, interruptible sleep (yielded to the driver
as `SyscallOutcome::Sleep`, serviced outside the shim lock in chunks that observe
process exit), and `sched_yield` yields the host thread.

**Single-threaded execution is unchanged and bit-identical to before.** The flip is
gated on `LinuxShim::threaded`, which the single-threaded corpus never trips, so its
deterministic virtual clock — and the differential oracle that depends on it — is fully
preserved.

## Consequences / bounded risk

- A threaded program's timing is now real and therefore **not reproducible**. This is
  acceptable because the acceptance program (`pthreads.elf`) produces a time-independent
  result (`400000`), and mt.rs's reference has a scripted fallback on non-x86 hosts.
- **The test corpus must never assert on threaded timing.** Any future threaded test
  whose *output* depends on wall-clock time would be inherently flaky — that is a
  property of real concurrency, not a bug in this design. New threaded tests must assert
  on time-independent observable output only.

## Alternatives considered

- **Keep the virtual clock in mt mode** — rejected: it makes `nanosleep` spin instead of
  block (no real yield between threads) and is non-deterministic under concurrent reads
  regardless, so it buys nothing while breaking real programs.
- **Switch to host time for all processes** — rejected: it throws away the determinism
  the entire differential corpus relies on, to fix a problem only threaded processes
  have.
