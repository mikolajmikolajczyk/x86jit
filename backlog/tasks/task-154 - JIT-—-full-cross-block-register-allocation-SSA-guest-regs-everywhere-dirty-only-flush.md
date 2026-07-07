---
id: TASK-154
title: >-
  JIT — full cross-block register allocation (SSA guest regs everywhere +
  dirty-only flush)
status: Done
assignee: []
created_date: '2026-07-07 13:06'
updated_date: '2026-07-07 13:43'
labels:
  - 'crate:cranelift'
  - 'goal:perf'
milestone: open-backlog
dependencies:
  - TASK-105
ordinal: 163000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Extend the register handling beyond what M5-T3e (task-105) shipped. CURRENT STATE: (a) only LOOP regions form (T3f policy) and carry the 16 GPRs + fuel as SSA Variables across their sub-blocks; single blocks and straight-line code stay WRITE-THROUGH — every WriteReg stores to CpuState immediately (gpr_cache is intra-block only). (b) Every region exit/trap FULLY flushes all 16 GPRs in ret(), even registers never written. FULL cross-block regalloc = (1) promote the guest register file (GPRs, and ideally flags) to SSA Variables in ALL compiled units incl. single-block, so intra-block/adjacent-block reg ops stay in host registers (cranelift regalloc2) instead of round-tripping CpuState on every write; (2) dirty-tracking flush — at an exit/trap store only registers written since entry, not all 16.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Run-side speedup (perf-bench v2 compile/run split) on register-heavy workloads (sha256, lua) with no interp==JIT divergence
- [ ] #2 Single-block reg ops no longer store_cpu on every WriteReg (kept in Variables, flushed at block end)
- [ ] #3 Dirty-only flush: an exit stores only the GPRs written since entry, verified by codegen inspection or a store-count test
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Refs: codegen.rs translate_block / translate_region, gpr_vars vs gpr_cache, ret() flush (~line 203-214). INTERPLAY / traps: §16 pitfall #0 instruction atomicity + the RIP-retry convention — a register flush must NOT be committed before a potentially-trapping op; the fault/trap path must still flush *current* state (checked_addr, RET_UNMAPPED, RET_MMIO_DEFER all set RIP then ret). Guard pages (GP) now fault mid-block via SIGSEGV->guarded_run, which does NOT go through ret() — so registers carried only in Variables would be STALE at a guard fault (this is the documented region-mode 'GPRs may be stale' residual, decision-7 / GP-3). Full regalloc widens that staleness to single blocks too, so either (i) keep the guard-fault register-precision residual documented (fine until task-123 builds a guest signal frame), or (ii) add deopt metadata. Bench: watch compile-cost increase vs run win (perf-bench v2). Sibling: BGT-6 (task-140) widens which code becomes regions.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
WON'T DO — abandoned after empirical investigation (2026-07-07). See decision-8. Premise (single-block write-through to CpuState is a bottleneck) is largely FALSE: CpuState stores are L1-cheap and cranelift/CPU absorb them. Two approaches measured (isolated A/B vs baseline 2a1c305 where the current write-through code is ~0%): (1) promote the guest register file to SSA Variables in single blocks too (the 'full' approach) REGRESSES fib32 +16% / sha256 +21% / sqlite+lua +20%+ — cranelift adds register pressure carrying block-spanning Variables with no reuse to amortize (loops/regions amortize it; straight-line code can't); gate BLOCKED. (2) a write-BACK cache (defer stores to a dirty-flush at exit, no Variables) is perf-NEUTRAL (fib32 -1..-3%, sha256 +-3%, all within noise bands; gate OK) — the deferred stores save ~nothing. Neither yields a meaningful single-block win, and both cost the single-block guard-fault GPR precision (guarded_single_block_fault_preserves_gpr_ordering) that task-123 will want. Reverted all code; write-through stays. Region mode already carries GPRs as Variables (M5-T3e) where the loop reuse pays off — that is the right and only place for it.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
