---
id: decision-6
title: >-
  Threaded clock v2: virtual monotonic value with real host blocking (supersedes
  decision-4's clock domain)
date: '2026-07-06 20:04'
status: proposed
---

**Deciders:** Mikołaj Mikołajczyk (architect consult: Fable 5)

> Drafted by Fable 5 for task-134; the maintainer ratifies (`proposed` →
> `accepted`) and, on acceptance, edits decision-4's status line to
> `Superseded by decision-6 (clock value domain; real blocking, single-threaded
> preservation, and the non-assertion rule carry forward)`. Design + exact code
> sites: `backlog/docs/design/threaded-clock-plan.md`.

## Context

Decision-4 anchored the mt-mode guest `CLOCK_MONOTONIC` to real host time at
the first `clone(CLONE_VM)` (`LinuxShim::now_ns`, shim.rs:747-755;
shim.rs:2377). That assumes the guest runs near real-time — true for the
interpreter on an idle host, false in general. Measured: two adjacent
`time.Now()` reads cost native 50 ns / interp 45 µs / eager-JIT 19 ms of host
wall-time. Under the host-anchored clock the guest perceives its own execution
as arbitrarily slow: Go net/http's deadlines/timers blow before a response is
written (the go-caddy P5 eager-JIT leg never serves), and the same mechanism
makes `go_http`/`go_net` load-flaky even on the interpreter (host load ~3.7 →
empty response). The failure is in the clock *value*, not in blocking —
accept/epoll/read/handler/serialization are proven correct
(httptest.Recorder output is byte-identical interp==jit).

Decision-4 rejected a virtual mt clock on two grounds: (1) a nanosleep that
only advances a counter doesn't block, so deadline pollers burn host CPU;
(2) concurrent reads of a shared counter are interleaving-dependent, so
determinism is lost anyway. Objection (1) conflated the clock's value with
its blocking; objection (2) is real but already neutralized by decision-4's
own non-assertion rule.

## Decision

**In mt mode, the guest reads a rate-controlled virtual monotonic value while
all blocking stays real.** One shared `AtomicU64` per threaded process
(`MtClock`, `Arc`-shared between `LinuxShim` and `ThreadShared`), seeded from
the single-threaded virtual clock at the flip (never a backward jump).
It advances from exactly three sources, all functions of guest behavior:

1. a fixed quantum per clock read (`MT_CLOCK_TICK_NS`, 10 µs — approximating
   the interpreter's measured read pacing), via atomic `fetch_add`;
2. the requested duration of a **completed** real sleep
   (`SyscallOutcome::Sleep`), credited by the driver as
   `fetch_max(entry + dur)` after the real block;
3. the timeout of an **expired** real wait (`FutexWait`/`EpollWait`
   timeouts), credited the same way on the timeout path only.

`nanosleep`/`clock_nanosleep` still yield `Sleep` (real host sleep), futex
still really parks/wakes on `ThreadShared`, `epoll_pwait` still really waits
on host epoll — unchanged driver servicing, so no busy-spin (answers
objection 1) and real I/O readiness keeps working. Credit-on-expiry makes
timers fire after at most one full real wait (Go's futexsleep/netpoll
re-sleep loop terminates). `fetch_max` (not `fetch_add`) for wait credits
keeps concurrent sleepers overlapping like real time. Monotonicity is
guaranteed by the atomic's RMW total order (answers the guest-visible half of
objection 2); reproducibility is *not* claimed — the mt clock is
rate-controlled, not deterministic, and stays safe under decision-4's
non-assertion rule, which this decision carries forward unchanged.

**Single-threaded execution is bit-identical to before** (the
`threaded == false` tick clock and the differential corpus that depends on
it, #13), also carried forward unchanged. A forked child gets a fresh clock
and restarts single-threaded, as today.

## Alternatives considered

- **Keep the host-anchored clock** (decision-4 as-is) — rejected: guest time
  races under any backend or host-load level that runs slower than realtime;
  breaks eager JIT permanently and flakes the interpreter under load.
- **Pure per-read virtual clock (no wait credits)** — rejected: an expired
  real wait wouldn't move the clock, so Go's timer M re-sleeps its full
  remaining interval on every wake — unbounded real latency per timer.
- **Pure wait-credit clock (no read quantum)** — rejected: a deadline loop
  that only reads the clock (`for time.Since(start) < d {}`) freezes time and
  spins forever — the #13 hazard.
- **Host-time governor** (virtual clock capped to real elapsed) — rejected as
  scope: reintroduces host-time coupling for a property (wall-pacing) nothing
  asserts; recorded in deferred.md.

## Consequences

- The go-caddy P5 eager-JIT leg becomes serviceable; `go_http`/`go_net` stop
  being load-flaky (perceived time is speed-invariant).
- A threaded guest's clock no longer tracks host wall-time at all: guests
  timing real host-fd I/O see virtual ≪ real elapsed. Acceptable under the
  non-assertion rule; the trigger to revisit is a real workload misbehaving
  on it (that would mean crediting blocking fd I/O too).
- The mt clock value remains non-reproducible run-to-run (interleaving); the
  test corpus must keep asserting only time-independent output — the
  decision-4 rule, restated.
- `MT_CLOCK_TICK_NS` is a tunable semantic constant; the eager-JIT go_http
  leg is its empirical gate.

## Trigger to revisit

A guest that legitimately needs wall-clock-correlated time (rate limiters,
TLS certificate validity, host-I/O timing) or a corpus need to assert on
threaded timing — either reopens the governor alternative.
