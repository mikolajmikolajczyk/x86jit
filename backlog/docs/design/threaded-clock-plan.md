---
id: doc-28
title: 'Threaded clock v2 — virtual monotonic value, real host blocking (VCLK)'
type: specification
created_date: '2026-07-06 20:30'
---

# Threaded clock v2 — virtual monotonic value, real host blocking (VCLK)

Ready-to-execute plan for task-134: replace the mt-mode host-anchored monotonic
clock (decision-4) with a **rate-controlled virtual monotonic clock** — the
*value* the guest reads is virtual (advances with guest progress, not host
wall-time), while `nanosleep`/`futex`/`epoll_pwait` keep **really blocking** the
host thread. Authored by Fable 5 (architect session, 2026-07-06), grounded in
the working tree at `f44406e`; exact sites cited below. Supersedes the clock
half of decision-4 — see the drafted decision-6.

**Framing.** The guest cannot be allowed to perceive host wall-time when its
execution speed varies ~4 orders of magnitude by backend (two adjacent
`time.Now()` reads: native 50 ns / interp 45 µs / eager-JIT 19 ms). Under the
host-anchored clock, Go net/http's deadline/timer machinery sees minutes pass
during a millisecond of guest progress and kills connections before the
response is written (go-caddy P5 JIT leg), and the same mechanism makes
`go_http`/`go_net` **load-flaky even on the interpreter** (host load ~3.7 →
interp runs < realtime → empty response in 0.34 s). The fix makes
guest-perceived time a function of **guest behavior** (clock reads issued,
sleep/timeout durations requested and really waited out), so every backend —
and every host-load level — perceives the same time per unit of guest
progress. This is orthogonal to background tier-up (doc-27), which smooths
compile stalls but does not change what the clock returns.

## Current shape (verified sites)

- **Two clock domains** in `LinuxShim` (`x86jit-linux/src/shim.rs`):
  - Single-threaded: `clock_ns: u64` (shim.rs:595), advanced `CLOCK_TICK_NS`
    (= 1 ms, shim.rs:186) per read in `now_ns` (shim.rs:747-755, the
    `None` arm), plus by the requested duration on `nanosleep` /
    `clock_nanosleep` (shim.rs:1738/1740). Deterministic — a pure function of
    the syscall sequence (#13; testing.md §9). Reported as
    `CLOCK_BASE_SEC + ns` by `tick_clock` (shim.rs:760-765), consumed by
    `SYS_CLOCK_GETTIME` (shim.rs:1694), `SYS_GETTIMEOFDAY` (shim.rs:1705),
    `SYS_TIME` (shim.rs:1641).
  - Threaded ("mt mode"): at the first accepted `clone(CLONE_VM)`,
    `clone_thread` flips `threaded` and sets
    `clock_anchor = Some((Instant::now(), clock_ns))` (shim.rs:2375-2378);
    thereafter `now_ns` returns `base_ns + anchor.elapsed()` (shim.rs:749) —
    **the racing path this plan removes**.
- **Real blocking** is already driver-side, via `SyscallOutcome`
  (shim.rs:35-94): `sleep_mt` (shim.rs:2410-2436) yields
  `Sleep(Duration)`; `futex_mt` (shim.rs:2314-2355) yields
  `FutexWait{timeout}`; `epoll_wait_mt` (shim.rs:2443-2471) yields
  `EpollWait{timeout}` (zero timeout serviced inline, shim.rs:2459-2463).
  The driver (`x86jit-linux/src/thread.rs`) services them **outside the shim
  lock**: `Sleep` in `FUTEX_POLL`-sized chunks observing `exited`
  (thread.rs:253-265), `FutexWait` via `ThreadShared::futex_wait` with a real
  `Condvar` deadline (thread.rs:93-120, 232-241), `EpollWait` via chunked
  host `epoll_wait` with a real `Instant` deadline (thread.rs:267-303).
- **Sharing**: N guest threads share `Arc<Mutex<LinuxShim>>` +
  `Arc<ThreadShared>` (thread.rs:160-174). `handle_mt` runs under the shim
  mutex and has no path to `ThreadShared` (by design — see the `next_tid`
  comment, shim.rs:596-600). Clock reads route through `delegate_mt`
  (shim.rs:2290) → the single-process `handle` → `tick_clock` → `now_ns`,
  which selects the domain on `clock_anchor` — so `now_ns` is the single
  choke point in both modes.
- **Fork**: the child shim inherits `clock_ns` but resets
  `threaded: false, clock_anchor: None` (shim.rs:863-867) — a forked process
  restarts single-threaded and deterministic.
- **`SYS_POLL`** reports instant readiness and consumes no time
  (shim.rs:2145-2167); mt guests (Go) block in epoll/futex, not poll.
- **Non-assertion rule** (decision-4, carried forward): no test asserts on
  threaded wall-clock output; threaded acceptance programs produce
  time-independent results (testing.md §12.3).
- **The current dodge**: `go_http.rs` passes its JIT leg only via
  `.tier_up(Some(50))` (x86jit-tests/tests/go_http.rs:59-64), which keeps
  Go's time-sensitive cold code interpreted. Note the task text says
  "un-ignore `go_http_serves_index_jit`", but the test is **not** `#[ignore]`d
  today — it is dodge-gated. The real acceptance gap is an **eager-JIT** leg
  (no tier-up), which currently fails.

## The model (settled — the design)

### M1 — One shared atomic virtual clock per threaded process

A newtype in `x86jit-linux/src/shim.rs` (next to the `CLOCK_*` consts — clock
semantics stay in one file, per the encode-traps-once rule):

```rust
/// Rate-controlled virtual monotonic clock for mt mode (decision-6). The value
/// is virtual — it advances with guest progress, never with host wall-time —
/// while all blocking stays real host blocking. Shared: the shim ticks it on
/// reads (under the shim lock); the driver credits it on expired waits
/// (outside the lock) — hence the atomic.
pub struct MtClock(AtomicU64);

impl MtClock {
    pub fn tick(&self, quantum: u64) -> u64 { self.0.fetch_add(quantum, Relaxed) + quantum }
    pub fn peek(&self) -> u64 { self.0.load(Relaxed) }
    /// Credit a completed wait: monotone, and concurrent sleepers overlap
    /// like real time instead of summing (fetch_max, not fetch_add).
    pub fn advance_to(&self, target_ns: u64) { self.0.fetch_max(target_ns, Relaxed); }
    pub fn seed(&self, ns: u64) { self.0.store(ns, Relaxed); }
}
```

Ownership: `LinuxShim` gains `mt_clock: Arc<MtClock>` (Default: zero);
`ThreadShared` gains `clock: Arc<MtClock>`. Wiring point: `run_threaded`
(thread.rs:160-164) owns the shim by value before Arc-wrapping it, so it
clones `shim.mt_clock` into `ThreadShared::new(..)` — no `handle_mt`
signature change, no shim→ThreadShared reference (preserving the P2.4
layering that keeps `next_tid` in the shim). The fork ctor (shim.rs:831-873)
gives the child a **fresh** `Arc<MtClock>` alongside its existing
`threaded: false` reset — per-process clocks, like today.

`Relaxed` suffices: the clock is a single atomic whose RMW operations
(`fetch_add`/`fetch_max`) have a per-location total modification order, and
no other data is published through it. That total order **is** the
monotonicity guarantee (see M5).

### M2 — What advances virtual time, and by how much

Exactly three sources — nothing else moves the clock:

1. **Per-read quantum** (shim side, under the shim lock). Every mt-mode clock
   read — `now_ns` with `threaded` set — does `mt_clock.tick(MT_CLOCK_TICK_NS)`
   and returns the ticked value. Readers: the `clock_gettime` /
   `gettimeofday` / `time` arms via `tick_clock`, and `sleep_mt`'s
   absolute-deadline math (shim.rs:2431). New const next to `CLOCK_TICK_NS`:

   ```rust
   /// mt-mode per-read quantum (decision-6). Smaller than the single-threaded
   /// 1 ms tick: Go's runtime reads the clock on every scheduler pass, and the
   /// quantum should approximate the interpreter's measured read pacing
   /// (~45 µs between adjacent time.Now() reads) so perceived time stays
   /// interp-like on every backend.
   const MT_CLOCK_TICK_NS: u64 = 10_000; // 10 µs — tunable, see open decision 2
   ```

2. **Completed real sleeps** (driver side). The `Sleep(dur)` arm: sample
   `entry = shared.clock.peek()` before blocking; after the chunked sleep runs
   to completion (not the `exited` early-out), `credit_expired_wait(entry, dur)`.

3. **Expired real timeouts** (driver side).
   - `FutexWait{timeout: Some(t)}`: when `futex_wait` returns `ETIMEDOUT`,
     `credit_expired_wait(entry, t)`. A wake (`0`) or value mismatch (`-EAGAIN`)
     credits nothing — the waker's own progress ticks the clock.
   - `EpollWait{timeout: Some(t)}`: when the deadline path sets `Rax = 0`,
     `credit_expired_wait(entry, t)`. Readiness before the deadline credits
     nothing; the `exited` early-out credits nothing.

   `entry` is sampled (`peek`, no tick) right after the shim guard drops,
   before blocking. The chunked loops (`FUTEX_POLL` backstop) credit only at
   final expiry, never per chunk.

`Yield` credits nothing. `peek` never ticks — only guest-visible reads pay
the quantum.

> **VCLK-2 correction — the credit is an idle-only CAS, not a `fetch_max`
> (decision-6, folded in at landing).** The rule above as first specified used
> `advance_to` (a `fetch_max`) for every expiry. Implementation + the eager-JIT
> acceptance gate exposed a fatal flaw: a **free-running periodic** waiter (Go's
> `sysmon`, a `time.Tick` loop) re-arms as many real waits as the awaited
> CPU-bound work permits, and crediting each one's full duration makes
> `Σ(credits) ≈ real elapsed` — so virtual time tracks host wall-time at ~1:1
> whenever any periodic timer runs, which for Go is always. That silently
> reintroduces exactly the decision-4 racing this design removes (measured: eager
> JIT still 100% empty-response at every quantum; `@10µs` even *regressed* the
> interp legs via read inflation). The fix — landed with VCLK-2 — is
> `credit_expired_wait` → `MtClock::try_advance_from(entry, entry + dur)`, a
> `compare_exchange` that credits **only when the clock still equals `entry`**,
> i.e. when the process was time-silent for the whole wait. Busy process: a
> worker's reads move the clock, the CAS fails, the timer fires on read-metered
> virtual time (true speed invariance). Idle process: nothing else moves the
> clock, the CAS succeeds, the timer fires after one real wait (M3 preserved —
> that is exactly the case the credits-off experiment proved load-bearing).
> `advance_to` survives as the idle-path primitive / test aid.

### M3 — Why timers fire: the credit-on-expiry argument

Go's runtime advances its timer heap via `nanotime()` (a `clock_gettime`
syscall here — no vDSO in the guest) and parks the waiting M in
`futexsleep(remaining)` or `epoll_pwait(delay)`, both derived from
`timer.when − nanotime()`. Under a **pure per-read** virtual clock, an
expired real wait would not move the clock: the M wakes after really waiting
`remaining`, reads `now < when` (only its own read quantum advanced), and
re-sleeps essentially the full `remaining` again — unbounded real time per
timer, shrinking only by one quantum per cycle. **Credit-on-expiry closes
this**: one real wait of `remaining` lands virtual `now ≥ when`, and the
timer fires after exactly one full real wait — the same real latency as
decision-4's host clock, without the racing value. Absolute
`clock_nanosleep(TIMER_ABSTIME)` lands on target the same way: `sleep_mt`
computes `dur = target − now` (shim.rs:2428-2431), the driver credits
`entry + dur ≈ target`.

### M4 — Why deadlines don't blow, and the busy-wait reconciliation

Virtual time elapsed across any stretch of guest execution is
`(#reads × q) + Σ(credited waits)` — a function of **guest behavior only**,
independent of host execution speed (the speed-invariance property). The eager
JIT compiling every block for 19 ms of wall-time between two reads now advances
the guest clock by exactly `q`, same as the interpreter; a loaded host slows
real progress but not perceived time.

> **Corrected (VCLK-2):** the original form here summed *every requested-and-
> waited* duration, which is **not** guest-behavior-only for a free-running
> periodic waiter — its number of expirations is a function of real time (see the
> M2 correction box). With the idle-only CAS credit, `Σ(credited waits)` counts
> only waits taken while the process was otherwise time-silent, restoring the
> property. Empirically: with the CAS gate (and the fixture-race fix below), the
> eager-JIT `go_http` leg serves correctly where it was 100% empty before.
>
> Note the two `go_http` failure modes were **not** one fix: the interp
> "load-flake" was a **non-clock** race in the acceptance fixture itself
> (`served=true` set before the response flush, then `os.Exit` on `Serve`'s
> return without waiting for `Shutdown`'s drain — it truncates to an empty close
> at native speed too), fixed in `httpserve.go` independently of the clock. Only
> the eager-JIT leg was the clock; and even it needs a *deadline-bearing* fixture
> to actually exercise the clock (the deadline-free fixture passes under the host
> clock once its own race is fixed — it is a driver-correctness test, not a clock
> gate). See VCLK-3.

The `for time.Since(start) < 30ms { n++ }` micro-repro issues one
`clock_gettime` per iteration (no vDSO), so it terminates in
`30 ms / q = 3000` iterations with `n > 0` on every backend — the #13
progress hazard is covered by the read quantum, exactly as it is for the
single-threaded clock. A deadline poll loop that never sleeps burns host CPU
for a *bounded* `deadline/q` iterations (each a syscall, so shim-lock paced);
decision-4's busy-spin objection applied to sleeps that didn't block, and
sleeps still block for real.

### M5 — Monotonicity and concurrency guarantees

- **Global monotone samples**: `tick` returns `old + q` from a `fetch_add`;
  `advance_to` is `fetch_max`. Both are monotone writes in the atomic's
  single modification order, so any read sample is ≥ every sample that
  precedes it in that order. Two guest threads whose reads are ordered by any
  guest-side happens-before (futex, memory) see non-decreasing values; one
  thread's consecutive reads **strictly** increase (each pays `q`).
- **No backward jump at the flip**: `clone_thread` seeds
  `mt_clock.seed(self.clock_ns)` where the anchor is set today
  (shim.rs:2377); the first mt read returns `clock_ns + q`, strictly above
  every single-threaded value.
- **No inflation ∝ thread count**: two sleepers of 10 ms starting at the same
  virtual instant land the clock at `entry + 10 ms` (fetch_max), not
  `entry + 20 ms` — concurrent waits overlap like real time.
- **Not deterministic — by design**: the value depends on read interleaving
  across threads. Decision-4's second objection is conceded, not solved: the
  mt clock is *rate-controlled*, not reproducible, and the standing
  non-assertion rule (testing.md §12.3) is what makes that safe. The
  guest-visible contract is monotonicity + progress, both guaranteed above.
- **Accepted skew**: credit lands at wait *expiry*, not entry, so a sibling
  can read `t` while a sleeper's credit is pending; the credit is monotone
  (`try_advance_from` only ever writes `entry + dur > entry`, and only when the
  value still equals `entry`), and on success the sleeper's own next read is
  ≥ `entry + dur`. On CAS failure (the busy case) the clock has already moved
  past `entry` under concurrent reads, so monotonicity holds trivially.

### M6 — What is deliberately out (record in deferred.md, do not implement)

- `SYS_POLL` stays instant-ready and time-free (shim.rs:2145) — Go blocks in
  epoll/futex; wire poll timeouts only when a real guest needs them.
- One clock domain: `CLOCK_REALTIME` == `CLOCK_MONOTONIC + CLOCK_BASE_SEC`,
  as today. No per-clock drift, no `CLOCK_THREAD_CPUTIME_ID`.
- No vDSO; no host-time governor ("max virtual speedup" rate limit) — nothing
  asserts wall-time pacing.
- Blocking host-fd I/O (a blocking socket `read`, shim.rs:2568-2578 comment)
  consumes no virtual time — real host waits on real fds are invisible to the
  guest clock. Bounded risk, see R3.

## Invariants (the checklist the tests pin)

- **I1 single-threaded bit-identity**: the `clock_anchor == None` /
  `threaded == false` paths are untouched — same `clock_ns` field, same
  `CLOCK_TICK_NS`, same nanosleep advancement (shim.rs:1738/1740). The
  differential corpus is the gate.
- **I2 monotone**: no guest thread ever reads a smaller value than any value
  it (or anything it synchronized with) previously read, including across the
  ST→mt flip.
- **I3 progress (#13)**: any deadline loop that reads the clock terminates in
  ≤ `deadline/q` reads.
- **I4 timer latency (idle)**: a guest wait of duration `d` that really expires
  **while the process is otherwise time-silent** advances virtual time by ≥ `d`
  — an idle timer fires after at most one full real wait. On a busy process the
  credit CAS fails and the timer instead fires on read-metered virtual time via
  the guest's own re-arm loop (Go rechecks `nanotime` after `ETIMEDOUT`/`Rax=0`
  and re-sleeps the remainder) — later in real time, but on-time in virtual time.
- **I5 speed invariance**: virtual elapsed over a guest code stretch is
  independent of backend and host load. Holds **because** credits are idle-only
  (the M2 correction box): summing every periodic expiry, as first specified,
  made virtual ∝ real for a free-running timer and broke this invariant — the CAS
  gate is what makes I5 true.
- **I6 real blocking**: `Sleep`/`FutexWait`/`EpollWait` outcomes and their
  driver servicing are unchanged in *when and how* they block — only the
  clock credit is added.

## Decision-4 revision: supersede, not amend

`backlog/docs/decisions.md` is explicit: substance of an accepted decision is
not edited; a reversal is a new decision that supersedes it, plus a status
back-link on the old file. This change reverses decision-4's core clause (the
mt clock *value* domain) while keeping three of its clauses alive (real
blocking, single-threaded preservation, the non-assertion rule) — that is a
substance change. **Recommendation: a new decision-6**, drafted at
`backlog/decisions/decision-6 - …` with status `proposed`; on ratification the
maintainer flips it to `accepted` and edits decision-4's status line to
`Superseded by decision-6 (clock value domain; real blocking, single-threaded
preservation, and the non-assertion rule carry forward)`.

## Phases (risk-ordered, each independently landable + testable)

### VCLK-1 — `MtClock` + plumbing (inert)

`MtClock` newtype + `MT_CLOCK_TICK_NS` const in shim.rs; `mt_clock:
Arc<MtClock>` field in `LinuxShim` (Default zero; fresh Arc in the fork ctor);
`clock: Arc<MtClock>` in `ThreadShared`, wired in `run_threaded`; seed at the
flip (shim.rs:2377) **alongside** the still-authoritative `clock_anchor`.
**No behavior change** — `now_ns` untouched. Gate: full suite unchanged;
unit tests for `MtClock` semantics (tick returns old+q; advance_to is
monotone max; concurrent tick/advance_to interleavings never yield a
decreasing sample).

### VCLK-2 — the switch (the feature lands)

`now_ns` mt arm returns `mt_clock.tick(MT_CLOCK_TICK_NS)`; delete
`clock_anchor` (field, flip write, fork-ctor init, doc comments at
shim.rs:605-609); driver credits in the `Sleep` / `FutexWait` / `EpollWait`
arms per M2; rewrite the flip unit test
(`clock_is_deterministic_until_threaded_then_anchors`, shim.rs:2914-2931) to
pin: ST tick unchanged, flip seeds the mt clock, mt reads tick `q`
monotonically, a credited wait advances ≥ its duration. Gate: mt.rs +
`go_http`/`go_net` interp and tiered-JIT legs green; full differential corpus
(`--features unicorn`, minus fuzz_robustness) green — that is the I1 proof;
clippy `-D warnings` + fmt.

### VCLK-3 — acceptance: eager-JIT leg + de-flake evidence

Add `go_http_serves_index_jit_eager` (no `.tier_up`) to
x86jit-tests/tests/go_http.rs — the case that races today — and update the
task-134 comment block (go_http.rs:109-117). Audit `go_net.rs`'s JIT leg the
same way. Keep the tiered leg (it exercises FD-TIER wiring). De-flake
evidence: run the interp legs under synthetic host load (e.g. `nice -n0
stress-ng --cpu $(nproc)` alongside) before/after and record the result in
task notes — load cannot be asserted in CI, so this is documented manual
verification. Optional (cheap, recommended): a threaded micro-guest asserting
*termination* of a `while (now < start+30ms) n++` loop on both backends —
termination-shaped, so it respects the non-assertion rule. Gate: eager leg
green on x86; ARM via the manual CI workflow.

### VCLK-4 — docs + ratification

Decision-6 `proposed → accepted` + decision-4 status back-link (maintainer);
`backlog/docs/status.md` (threaded clock now virtual); `deferred.md` entries
from M6; `architecture.md`/`glossary.md` if they mention the mt clock;
close task-134 with the tier-up dodge decision (open decision 3) resolved.

## Risks

- **R1 quantum too large → time inflation**: Go's scheduler is read-heavy;
  at 1 ms/read a request's few hundred reads become virtual seconds and
  short internal deadlines could fire spuriously — the *same* empty-response
  symptom, opposite cause. Bounded: the eager-JIT leg is the acceptance
  gate, and the knob is one const. 10 µs ≈ the measured interp read pacing.
- **R2 quantum too small → slow deadline loops**: a `deadline/q`-iteration
  poll loop costs real syscall time per iteration. At 10 µs a 30 ms deadline
  is 3 k syscalls — negligible. Only pathological guests (hours-long
  poll-only deadlines) would hurt; they'd hurt today too.
- **R3 real-vs-virtual divergence around host fds**: a guest timing a
  blocking host `read`/accept sees virtual ≪ real elapsed. Non-asserted by
  the corpus rule; revisit only if a real workload misbehaves (that would be
  the trigger to credit blocking fd I/O too).
- **R4 credit races**: reads tick under the shim lock while credits land
  outside it — the atomic's RMW total order makes every interleaving
  monotone (M5); a targeted unit test pins it. No lock-order change: the
  driver touches only the `Arc<MtClock>`, never the shim, while blocked.
- **R5 in-flight BGT work**: `mt.rs`, `bg_tier.rs`, `cache.rs` carry
  uncommitted BGT-4 changes in the working tree — land order must be
  coordinated (VCLK is code-orthogonal to BGT; the only shared file is
  `mt.rs` if VCLK-3 adds a micro-guest there).
- **R6 escalation path coverage**: `run_threaded` is entered both directly
  (tests, OCI runner) and via deferred-scheduler escalation; the wiring in
  VCLK-1 sits in `run_threaded` itself, so both entries get the shared clock
  — verify with the OCI multiprocess suite in VCLK-2's gate.
- **R7 sparse-reader timer starvation (from the CAS gate)**: with idle-only
  credits (M2 correction), a process whose *only* clock reader reads far apart in
  real time, while a short periodic timer runs, makes that timer's firing latency
  ≈ `(period / q) × read-spacing` of real time — the timer advances by `q` per
  read instead of jumping the full period. Bounded and not observed: Go's runtime
  is read-dense (sysmon + scheduler), so the CAS almost always fails *because time
  is already moving*, and idle processes (sparse readers) have no competing timer
  to starve. Escape hatch if a real workload ever hits it: a fractional fallback
  on CAS failure (`fetch_max(entry + dur/K)`), reintroducing coupling at `1/K`
  rate — do **not** build speculatively; the corpus does not need it. (Also: a raw
  `nanosleep` whose CAS fails returns without its time claim; Go's only user is
  `runtime.usleep` pacing, which never re-checks the clock — acceptable.)

## Open decisions for the maintainer

1. **Supersede vs amend decision-4**: recommendation is supersede via
   decision-6 (drafted, `proposed`) — decisions.md forbids editing an
   accepted decision's substance. Ratify or redirect.
2. **`MT_CLOCK_TICK_NS` value**: 10 µs recommended (interp-pacing argument,
   M2/R1-R2); 1 µs / 100 µs / 1 ms are the alternatives. The eager go_http
   leg is the empirical gate; tune in VCLK-3 if needed.
3. **The tier-up dodge in `go_http_serves_index_jit`**: keep
   `.tier_up(Some(50))` on the existing leg (it exercises FD-TIER) and add a
   separate eager leg (recommended), or strip the dodge from the existing
   leg. Same choice for `go_net.rs`.
4. **`Yield` credit**: recommended none (`sched_yield` is not a time claim);
   flag only because Go's `osyield` spins call it in tight loops — if a real
   guest ever spins on yield+clock-read, the read quantum already covers it.
5. **Micro-repro guest in VCLK-3**: worth the corpus slot, or is the eager
   go_http leg sufficient evidence? (Recommended: add it — it is the minimal
   regression tripwire for I3/I5 and costs ~30 ms of virtual time.)
