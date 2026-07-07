---
id: decision-8
title: >-
  Single-block guest registers stay write-through — SSA-Variable promotion
  measured as a regression
date: '2026-07-07 13:43'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk

## Context

task-154 proposed "full cross-block register allocation": carry the guest register
file as Cranelift SSA `Variable`s in **every** compiled unit (not just loop regions,
which already do this — §12 M5-T3e) and flush lazily, on the theory that the
single-block path's write-through (`store_cpu` on every guest register write) is a hot
round-trip worth eliminating.

Investigated empirically (2026-07-07). Isolated A/B via `x86jit-bench` `gate`, each
variant vs the same baseline `2a1c305`, on the same host — where the shipped
write-through code measures ~0% (fib32 +0.3%, sha256 −1.1%).

## Decision

**Keep single-block guest registers write-through. Do not promote them to SSA
Variables.** The premise is largely false — `CpuState` register stores are L1-cheap and
the host CPU / Cranelift absorb them; the round-trip is not a bottleneck.

Two implementations were measured and reverted:

- **Variables in single blocks (the "full" approach) — REGRESSION.** fib32 +16%,
  sha256 +21%, sqlite/lua +20%+; the perf gate blocked. Cause: Cranelift carries the
  16 guest GPRs as `Variable`s live across the whole block, adding host-register
  pressure (only ~14 usable GPRs on x86-64) with **no reuse to amortize** it.
  Loops/regions amortize the same cost across iterations — which is exactly why region
  mode (M5-T3e) uses Variables and single blocks should not.
- **Write-back cache (defer the stores to a dirty-flush at exit, no Variables) —
  NEUTRAL.** fib32 −1…−3%, sha256 ±3%, all inside the noise bands; gate OK. The
  deferred stores save essentially nothing over write-through.

Neither yields a meaningful single-block speedup, and both would trade away the
single-block **guard-fault GPR precision** (the guard-page SIGSEGV path
`siglongjmp`s past the register flush, so Variable/write-back GPRs go stale in
`CpuState` — pinned by `guarded_single_block_fault_preserves_gpr_ordering`), which
task-123 (a guest signal frame built from a JIT fault) will want.

## Consequences

- Single blocks keep the `gpr_cache` write-through path unchanged; region mode keeps
  its SSA-`Variable` carry (M5-T3e). Register-in-host-register optimization lives only
  where loop reuse pays for it.
- task-154 is closed **won't-do** with this decision as the record, so the idea isn't
  re-attempted blind.
- The guard-fault GPR-precision residual stays **single-block-precise / region-stale**
  (decision-7), unchanged.
- Real JIT run-side wins should be sought elsewhere — e.g. task-155 (guard pages
  eliminate the per-access bound check), or widening region formation (BGT-6, task-140)
  so more hot code runs where Variable carry already helps.

## Links

- task-154 (closed won't-do) · [[decision-7]] (guard-fault precision residual).
- `x86jit-cranelift/src/codegen.rs` (`gpr_cache` write-through; `gpr_vars` region carry).
- `x86jit-bench` perf gate; baseline `2a1c305`.
