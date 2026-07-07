---
id: decision-6
title: >-
  Threaded clock v2: virtual monotonic value with real host blocking (supersedes
  decision-4's clock domain)
date: '2026-07-06 20:04'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk (architect consult: Fable 5)

> Drafted by Fable 5 for task-134; **ratified by the maintainer 2026-07-07** after
> the VCLK-2 implementation + acceptance (the idle-only CAS credit correction below
> is part of what is ratified). [[decision-4]]'s status line is set to `superseded`
> with the carry-forward clauses noted. Design + exact code sites:
> `backlog/docs/design/threaded-clock-plan.md`.

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

1. a fixed quantum per clock read (`MT_CLOCK_TICK_NS`, 100 ns — see the tuning
   note below), via atomic `fetch_add`;
2. the requested duration of a **completed** real sleep
   (`SyscallOutcome::Sleep`), credited by the driver after the real block;
3. the timeout of an **expired** real wait (`FutexWait`/`EpollWait`
   timeouts), credited the same way on the timeout path only.

**Wait credits use an idle-only CAS gate, not `fetch_max`** (the correction
below): the credit `MtClock::try_advance_from(entry, entry + dur)` lands only
when the clock still equals `entry` — i.e. no other guest thread moved it
during the wait. On a busy process the workers' own reads carry virtual time
forward and the CAS fails, so a free-running periodic timer fires on
read-metered virtual time; on an idle process nothing else moves the clock, the
CAS succeeds, and the timer fires after one real wait.

`nanosleep`/`clock_nanosleep` still yield `Sleep` (real host sleep), futex
still really parks/wakes on `ThreadShared`, `epoll_pwait` still really waits
on host epoll — unchanged driver servicing, so no busy-spin (answers
objection 1) and real I/O readiness keeps working. Idle credit-on-expiry makes
timers fire after at most one full real wait (Go's futexsleep/netpoll
re-sleep loop terminates). Monotonicity is guaranteed by the atomic's RMW
total order (answers the guest-visible half of objection 2); reproducibility is
*not* claimed — the mt clock is rate-controlled, not deterministic, and stays
safe under decision-4's non-assertion rule, which this decision carries forward
unchanged.

### Correction, discovered at implementation (VCLK-2)

The wait credit was first specified as an unconditional `fetch_max(entry + dur)`
for every completed/expired wait. Implementation plus the eager-JIT acceptance
gate (architect review by Fable 5) exposed that this **re-couples virtual time
to host wall-time** for any *free-running periodic* waiter (Go's `sysmon`, a
`time.Tick` loop): such a waiter re-arms as many real waits as the awaited
CPU-bound work permits, so `Σ(credits) ≈ real elapsed` — silently reinstating
exactly the decision-4 racing this decision removes (measured: eager-JIT
`go_http` stayed 100% empty at every quantum; a 10 µs quantum even *regressed*
the interpreter legs via read inflation). The **idle-only CAS gate above is the
fix** — it credits only a genuinely idle wait, which is the M3 progress case the
"credits disabled → hang" experiment proved load-bearing, and defeats the
re-coupling on busy processes. `MT_CLOCK_TICK_NS` was lowered to **100 ns**
(swept; smaller avoids read inflation, larger risks it). `advance_to`
(`fetch_max`) survives as the idle-path primitive. Design detail:
`backlog/docs/design/threaded-clock-plan.md` (M2 correction box, R7).

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

- The go-caddy P5 eager-JIT leg becomes serviceable (perceived time is
  speed-invariant, so Go's runtime machinery no longer sees minutes pass during
  a slow compile). Note the `go_http` interpreter *load-flake* turned out to be
  a **separate, non-clock** bug — an exit-before-flush race in the acceptance
  fixture (`httpserve.go` set `served=true` before net/http flushed, then exited
  on `Serve`'s return without awaiting `Shutdown`'s drain) — fixed independently.
  The deadline-free eager leg therefore passes under the host-anchored clock too;
  it is a driver-correctness test, and the CAS gate's speed-invariance is pinned
  by a unit test (`busy_process_expiry_does_not_credit`), the corpus having no
  long-span-deadline workload that would otherwise distinguish the credit rules.
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
