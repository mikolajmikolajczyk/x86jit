---
id: TASK-197.1
title: 'MODE-A.1: plumb CpuMode — Vcpu field, dispatcher, BlockKey(addr, mode)'
status: Done
assignee: []
created_date: '2026-07-10 10:31'
updated_date: '2026-07-10 12:45'
labels:
  - guest-modes
dependencies: []
parent_task_id: TASK-197
ordinal: 222000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Activate seams §17.3 + §17.4: `CpuMode` gains `Compat32`; the mode becomes a Vcpu/Vm construction parameter threaded through dispatcher -> cache -> `lift_block`/`lift_one` (today hardcoded `CpuMode::Long64` at lift.rs:71/132). Cache key becomes `BlockKey { guest_addr, mode }` (TASK-46 left the marker). `disasm.rs` takes the mode too. No 32-bit semantics yet — Long64 behavior must be bit-identical after the refactor.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Block cache key includes mode; a unit test shows the same guest addr yields distinct entries per mode
- [x] #2 Existing 64-bit suite passes unchanged
- [x] #3 Decoder bitness comes from the threaded CpuMode everywhere; no hardcoded Long64 outside Vm construction defaults — pinned by a test driving lift_block/lift_one through an explicitly passed mode
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed on feat/mode-a @ 92f457e. Pure refactor activating §17.3/§17.4 seams; 64-bit bit-identical (full unicorn diff suite 409/409, minus fuzz).

WHAT:
- CpuMode: added Compat32 (bits()==32) + Hash derive. Doc reframed as decode/lift context per the pre-work design decision.
- lift_block/lift_one/lift_region and disasm::disassemble/print_disassembly now take mode; dropped hardcoded Long64 at Decoder::new (lift.rs & disasm.rs).
- BlockKey{ guest_addr, mode } added in cache.rs (Copy/Eq/Hash), exported from lib.rs. TranslationCache re-keyed on BlockKey for: map, spans, hotness, region_decision, tier_pending. get/insert/upgrade/upgrade_region/bump_hotness/region_decision/set_region_decision/try_begin_tier_up/end_tier_up now take BlockKey. invalidate_overlapping returns Vec<BlockKey> and stays ADDRESS-scoped (drops every mode on a written page).
- Vm gained a mode field (default Long64) + set_cpu_mode/cpu_mode (mirrors set_guest_cpu_features house pattern). Vcpu gained a mode field, set in new_vcpu. resolve(vm,pc,mode) builds the key; step_one(mem,cpu,mode,scratch) and jit_abi::run_compiled(...,mode) thread it. drain_tier_up builds BlockKey from vm.mode (a Vm is single-mode, so TierUpRequest/Finished stayed pc:u64 — no public-API churn there).

LOUD REJECTION (§17.7): chose Vm::set_cpu_mode as the single reject point — it panics on Compat32 with a message pointing at 197.2/197.3. Free-standing lift_block/lift_one/disassemble accept Compat32 (decode only), so plumbing is real and unit-testable while nothing can silently RUN 32-bit yet.

TESTS: cache::same_addr_distinct_entry_per_mode (AC#1); lift::lift_bitness_comes_from_mode_argument drives lift_block+lift_one under explicit Long64 vs Compat32 (AC#3); vm::vm_defaults_to_long_mode + set_cpu_mode_rejects_compat32.

FOR 197.2/197.3/197.5 (branch off 92f457e):
- Decoding at bitness 32 already works mechanically. What's NOT done: 32-bit execution semantics (addr truncation/wrap, EIP wrap, push/pop/call/ret widths). Those are gated behind the set_cpu_mode panic — REMOVE/loosen that assert as each mode's semantics land.
- Runtime predictor state (Vcpu.fast, ibtc_refills, ret_stack) is still keyed by raw RIP, intentionally: a running Vm is single-mode so no cross-mode aliasing. Revisit only if runtime mode switching is ever added (explicitly deferred, §17.6).
- Segment bases deliberately NOT key material (per design note #2): follow the FS/GS runtime-read pattern in effective_address.
- CpuMode is now in the flattened re-exports (x86jit_core::CpuMode).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
