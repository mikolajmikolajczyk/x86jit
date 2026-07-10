---
id: TASK-197.1
title: 'MODE-A.1: plumb CpuMode — Vcpu field, dispatcher, BlockKey(addr, mode)'
status: To Do
assignee: []
created_date: '2026-07-10 10:31'
updated_date: '2026-07-10 10:37'
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
- [ ] #1 Decoder bitness comes from the threaded CpuMode everywhere; no hardcoded Long64 outside Vm construction defaults
- [ ] #2 Block cache key includes mode; a unit test shows the same guest addr yields distinct entries per mode
- [ ] #3 Existing 64-bit suite passes unchanged
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Design decision (pre-work, 2026-07-10): CpuMode variant semantics = DECODE/LIFT CONTEXT, not the architectural mode register.

1. BlockKey stays { guest_addr, mode } forever; anything that changes how the same bytes decode or lift (effective operand/address-size default, i.e. mode x CS.D) becomes a NEW VARIANT. Future protected mode adds Protected16 and Protected32 as separate variants — key shape never changes, no aliasing, no redesign.
2. Segment bases are NOT key material. Follow the existing FS/GS pattern: base lives in CpuState, effective_address emits a runtime read. Real16 (base = sel*16 on segreg write) and future descriptor-cache bases both fit — segment reloads never invalidate translations.
3. Known deferred tail: SS.B (16-bit code with 32-bit stack) — decide variant-vs-runtime-state when a consumer arrives; do not design now (spec 17.6).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
