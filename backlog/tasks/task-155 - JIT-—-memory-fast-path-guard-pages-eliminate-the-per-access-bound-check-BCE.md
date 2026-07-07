---
id: TASK-155
title: 'JIT — memory fast path: guard pages eliminate the per-access bound check (BCE)'
status: Done
assignee: []
created_date: '2026-07-07 13:06'
updated_date: '2026-07-07 14:10'
labels:
  - 'crate:cranelift'
  - 'goal:perf'
milestone: open-backlog
dependencies:
  - TASK-127
ordinal: 164000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
spec §8.2.3 (spec.md:1037,1085) planned guard pages as the 'measured M5 perf optimization' that REPLACES the explicit per-access bounds check. Guard pages now exist (doc-30 / decision-7, GP-1..GP-5): host-backed spans mmap the holes PROT_NONE and a fault becomes Exit::UnmappedMemory via SIGSEGV->guarded_run. Cash it in — two wins: (1) GUARD-PAGE BCE: for a host-backed guarded span drop checked_addr's software span-bound (icmp end>memsize + overflow + brif to the fault stub) on the hot path and rely on hardware. (2) REDUNDANT-CHECK ELIMINATION within a block: dedup dominated checks so N accesses to a proven-safe frame ([rbp-k], same base+window) don't each re-check.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Run-side speedup (perf-bench v2) on memory-heavy workloads (memcpy/string, sqlite, djpeg) with no compile-cost regression that eats the win
- [ ] #2 interp==JIT preserved incl. the guard_pages.rs fault pins: a wild IN-span AND a wild OUT-of-span pointer both surface Exit::UnmappedMemory (not demand-zero, not an honest crash)
- [ ] #3 MMIO-defer path and faulting-RIP (GP-3 CodeMap) + access kind (D4) still correct; Vec-backed VMs keep the software check (no guards)
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Refs: checked_addr codegen.rs:2098 (the icmp+brif to drop); guard pages doc-30, x86jit-linux/src/sigsegv.rs (classifier: si_addr in span -> recover, else honest crash), memory.rs reprotect, hostmem::reserve_guarded; spec §8.2.3 (recommended bounds-check-first, guard-pages-as-M5-perf). KEY DESIGN GAP: today only IN-span-unmapped faults convert; an access BEYOND host_base+size (a truly out-of-bounds guest addr) hits unmapped HOST memory -> si_addr outside the span -> honest crash, which is WRONG if we remove the software span-bound (a guest OOB must trap, not kill the host). So (1) requires either a guard REDZONE mmap'd above the span (and page 0 already guarded) so any OOB host access lands in a classified guard within a known window, OR extending the SIGSEGV classifier to treat [host_base+size, host_base+size+redzone) as a guest OOB -> Exit::UnmappedMemory. Bound the redzone to the max single-access size + displacement. (2) is a pure codegen dominator/BCE pass, independent of (1) and safe on Vec-backed too. Land (2) first (portable), then (1) (guarded spans only). Note GP already removed the DEMAND-ZERO gap; this task removes the CHECK COST.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Delivered PARTIAL: RMW same-EA dedup only. See decision-9. checked_addr keeps a block-local checked_ea cache: a non-atomic RMW (add [mem],rax = Load+Store on one EA value) reuses the load's checked host pointer for the store, dropping one bound-check branch. Correct-by-construction (fewer branches, short-lived pointer = no register pressure, load's read-fault first = faithful); 116 differential/atomics/whole_program tests green. The BIG win (guard-page BCE — dropping the check) is UNSAFE for 64-bit: a guest addr>=span makes host_base+addr hit possibly-mapped host memory -> silent corruption, not a trap; guard pages only cover in-span holes (doc-30). Hoisting base/size loads REGRESSES (register pressure, task-154 pattern) -> reverted. Ceiling is real (~5-26% from an unsafe no-bounds experiment) but its cost is the per-access optimization barrier, only removable via unsafe trapping loads. Micro-opts below this host's noise floor (A/B passes disagreed 6-18% at loadavg 3-9). Next real win: BGT-6 (task-140) widening regions.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
