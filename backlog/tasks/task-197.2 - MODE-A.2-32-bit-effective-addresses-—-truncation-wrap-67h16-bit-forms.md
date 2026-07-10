---
id: TASK-197.2
title: 'MODE-A.2: 32-bit effective addresses — truncation/wrap + 67h(16-bit) forms'
status: In Progress
assignee: []
created_date: '2026-07-10 10:32'
updated_date: '2026-07-10 11:36'
labels:
  - guest-modes
dependencies:
  - TASK-197.1
parent_task_id: TASK-197
ordinal: 223000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
In Compat32 `effective_address` computes mod 2^32 (base+index*scale+disp wraps at 4 GiB, result zero-extended for the flat Memory lookup); the 67h prefix selects 16-bit addressing (mod 2^16, classic ModRM forms — iced decodes them, the lowering must not assume SIB). Stays a change inside the single helper per seam §17.5. RIP-relative does not exist in 32-bit mode — guard that path by mode.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Address arithmetic wraps at 32 bits in Compat32 (unicorn-diffed, incl. negative displacement wrap cases)
- [x] #2 67h-prefixed 16-bit addressing forms compute correctly (unicorn-diffed)
- [x] #3 lea honours address-size truncation without adding segment bases (unicorn-diffed, incl. a seg-prefixed lea case)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed on feat/mode-a-ea @ 96e92e9. Confined to effective_address_no_segment (seam §17.5); zero IR ops added, no interp/codegen changes, no push/pop/branch touches.

WHAT:
- Address-size truncation now keyed on iced's base/index register SIZE (mode-derived at decode, verified via probe): size-4 regs -> mask mod 2^32 (Compat32 default AND long-mode 0x67); size-2 regs -> mask mod 2^16 (0x67 16-bit forms [bx+si]/[bp+di+disp]/[bx], no SIB). Pure [disp16]/[disp32] absolute (base==index==None) needs no mask: iced hands a disp already sized to the decode width.
- RIP/EIP ip-rel: iced folds RIP+disp only at 64-bit decode; a 32-bit decode's ModRM disp32 is absolute (base==None). Added debug_assert_ne!(code_size, Code32) on both RIP and EIP branches so a Compat32 ip-rel operand fails loudly instead of computing garbage.
- lea unchanged mechanically: shares effective_address_no_segment, so it gets the truncation and still never adds the segment base (with_segment only wraps the access path).

WHY register-size, not threaded mode: iced encodes addressing width unambiguously in the operand register sizes (Long64=8, 32-bit=4, 16-bit=2). Keying on that keeps the change inside the single helper — threading CpuMode through ~40 effective_address callers/lift_* signatures would cross the 197.3 push/pop fence. The one thing needing mode (loud RIP guard) uses insn.code_size() carried on the instruction itself, no threading.

TESTS: new x86jit-tests/tests/addr32.rs (unicorn-gated, self-contained, UC_MODE_32). Direct interp+JIT vs Unicorn, no dependence on the 64-bit harness so cases port cleanly onto 197.5's lane. AC#1: addr32_wraps_at_4gib, addr32_negative_disp_wrap, addr32_base_index_scale_wrap. AC#2: addr16_bx_si_wraps_mod_64k, addr16_bp_di_disp, addr16_disp16_absolute (hand-encoded 67 8b 06 disp16). AC#3: lea32_truncates_address, lea32_ignores_segment_base (live fs_base=0x5000).

VERIFY: cargo nextest --features unicorn (minus fuzz_robustness) 419 passed / 2 skipped; clippy --all-targets --all-features -D warnings clean; fmt --check clean. 64-bit corpus bit-identical (debug_assert never fired).

FOR INTEGRATION/197.x: touched ONLY effective_address_no_segment + one iced import (CodeSize) in lift.rs; nothing else in lift.rs. To actually RUN an end-to-end Compat32 Vm you still need to loosen Vm::set_cpu_mode's Compat32 panic (197.1's single reject point) — addr32.rs sidesteps it by driving lift_block/interpret_block/run_compiled directly with CpuMode::Compat32.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
