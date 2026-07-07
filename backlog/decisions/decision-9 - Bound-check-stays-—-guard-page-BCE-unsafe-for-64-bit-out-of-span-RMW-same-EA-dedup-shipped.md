---
id: decision-9
title: >-
  Bound check stays — guard-page BCE unsafe for 64-bit out-of-span; RMW same-EA
  dedup shipped
date: '2026-07-07 14:09'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk

## Context

task-155 (spec §8.2.3, spec.md:1037/1085) proposed cashing in guard pages (doc-30) as
the "measured M5 perf optimization" that replaces the explicit per-access bounds check
(`checked_addr`: load `MemCtx.size`, `end = addr+size`, two `icmp`s + `bor` + `brif` to a
fault stub, then `base + addr`).

Investigated empirically (2026-07-07). An unsafe `X86JIT_UNSAFE_NOBOUNDS` experiment
(emit the raw `base + addr`, no check) measured the **ceiling**: sha256 ≈ −16%, sqlite
≈ −26%, lua ≈ −9%, fib32 ≈ −5% (JIT run-side vs checks-on). The cost is **real and
large** — but it is the check's *optimization barrier*, not the branch's execution: the
per-access CLIF block split stops Cranelift from keeping values in registers across the
access, reordering, or combining loads. Capturing it means removing the block split
(trapping loads), i.e. guard pages.

## Decision

**Keep the per-access software bound check. Guard-page BCE (dropping the check) is not
safely achievable for x86jit's 64-bit guests.** Ship only the safe sub-optimization:
redundant-check elimination within a block.

- **Guard-page BCE is unsafe (rejected).** Guard pages (doc-30) fault on *in-span*
  holes — that closes decision-3's demand-zero gap and already shipped. They cannot
  cover *out-of-span*: a guest address `>= span` makes `host_base + addr` land outside
  the mmap, and for an arbitrary 64-bit wild pointer that target can be **mapped host
  memory** (the JIT arena, another mmap, the stack) → **silent host corruption**, not a
  trap. No redzone bounds the full 64-bit range, and the classifier can't tell a guest
  OOB from a genuine host bug. So the bound check is load-bearing for out-of-span
  safety and for the `interp == JIT` trap invariant. spec §8.2.3's "guard pages replace
  the bound check" assumed a bounded guest; it does not hold here.
- **Hoisting the `MemCtx.base`/`size` loads out of the per-access path — regression
  (rejected).** Keeping them in registers across the block adds host-register pressure
  (same failure mode as task-154 / decision-8); Cranelift prefers to rematerialize the
  L1-hot reload. Measured slower.
- **RMW same-EA dedup — shipped (safe).** A non-atomic read-modify-write
  (`add [mem], rax`) lifts to `Load`+`Store` on the *same* effective-address value; the
  store reuses the load's already-checked host pointer instead of re-emitting the check
  + branch. Correct-by-construction (strictly fewer emitted branches, the cached
  pointer is short-lived so no register pressure; the load's read-fault is what x86
  raises first, so the skipped store check is faithful). Helps RMW-heavy guests
  (sqlite in-place updates, `inc`/`add [mem]`); below this host's measurement noise
  floor on the four micro-benchmarks (A/B passes disagreed 6–18% at loadavg 3–9).

## Consequences

- `checked_addr` keeps its bound check; a block-local `checked_ea` cache dedups the
  RMW `Load`+`Store` pair. Vec-backed and host-backed spans both keep the check.
- The "eliminate the per-access bound check" idea is **settled unsafe** — not to be
  re-attempted for 64-bit guests without a bounded-guest address mode.
- Real JIT run-side wins now sit with **widening region formation** (BGT-6, task-140):
  region mode already amortizes register carry over loops, which is where Cranelift's
  optimizer has room the bound check otherwise blocks.
- A confident measurement of sub-5% JIT changes needs a quieter host than the dev box
  (the perf gate's noise band is 4–28% here).

## Links

- task-155 (RMW dedup delivered) · doc-30 / [[decision-7]] (guard pages, in-span only) ·
  [[decision-8]] (register pressure, the sibling negative result).
- `x86jit-cranelift/src/codegen.rs` (`checked_addr`, `checked_ea`); spec §8.2.3.
