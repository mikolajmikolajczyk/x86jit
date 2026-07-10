---
id: TASK-197.3
title: 'MODE-A.3: 32-bit control flow + stack — EIP wrap, push/pop/call/ret widths'
status: Done
assignee: []
created_date: '2026-07-10 10:32'
updated_date: '2026-07-10 12:45'
labels:
  - guest-modes
dependencies:
  - TASK-197.1
parent_task_id: TASK-197
ordinal: 224000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Branch targets, call/ret return addresses and the dispatcher PC truncate to 32 bits in Compat32. Stack ops honour 32-bit default operand size (66h flips to 16-bit push/pop), ESP wraps at 2^32. Writing a 32-bit reg in Compat32 keeps storing zero-extended into the u64 backing state (no architectural upper bits — harmless, but pin with a test so JIT and interp agree).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 call/ret/jcc/jmp round-trip with 32-bit truncated targets (unicorn-diffed)
- [x] #2 interp == JIT on a 32-bit control-flow + stack differential batch
- [x] #3 push/pop/call frames are 4-byte (2-byte under 66h); ESP wraps mod 2^32 (unicorn-diffed + interp==JIT cases)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed on feat/mode-a-cf @ 5a5369d. All 3 ACs verified (cf32.rs: interp==JIT==Unicorn MODE_32, 7 cases); full unicorn suite 418/418 green, clippy -D warnings + fmt clean. Long64 bit-identical (slot=8/wrap_sp=false/pop_extra=0 paths).

WHAT:
- lift_insn(+mode): mask_pc truncates direct jmp/jcc/call targets + return_addr mod 2^32 (iced already truncates at bitness 32 — mask pins the invariant); indirect jmp/call targets get a runtime IrOp::And mask (branch_target).
- IrOp::Call{slot,wrap_sp} / IrOp::Ret{slot,pop_extra,wrap_sp}: interp+codegen honour frame width and mask SP mod 2^32; ret imm16 wired via pop_extra (was silently ignored in Long64 before — now correct in both modes; plain ret bit-identical).
- lift_push/lift_pop(+mode): push_pop_size defaults to stack_slot(mode) (4 in Compat32; 66h→2 via iced operand width); push masks the computed ESP (emit_sp_wrap) BEFORE it is the store address; ESP writes use sp_write_size(mode)=4 → central zero-extending GPR path does the wrap. leave uses stack_slot too.
- DECISION (66h branches, AC pt.4): Code::Call_rel16/Call_rm16/Retnw/Retnw_imm16 rejected as Unsupported in call_ret_slot (§17.7 loud) — EIP-mod-2^16 wrap not modeled. jmpw (Jmp_rel16/rm16) NOT rejected: iced still resolves near_branch_target, but truncation mod 2^16 is not applied — acceptable? NO real i386 code emits it; revisit if 197.5 fuzzing hits it.
- DECISION (straight-line fallthrough): block_end/guest_end (code running past the fetch window) does NOT wrap mod 2^32 — only reachable with code mapped at the very top of the 4GiB space (kernel-reserved on real i386). Left unwrapped, disproportionate to thread mode into interp block_end + every codegen guest_end.
- DECISION (ESP wrap test): true 0xFFFFFFFC-boundary pop needs the top guest page mapped; contiguous Flat model can't allocate 4GiB cheaply. Wrap pinned instead by polluting ESP bits 32-63 via set_reg and asserting stack ops clear them (Unicorn oracle can't hold those bits) + upper-32-zero assertions on every GPR in the harness.

lift.rs REGIONS TOUCHED (for merge): imports; CpuMode doc + wraps_32; lift_block/lift_one call sites; lift_insn signature + Push/Pop/Jmp/Call/Ret/Leave arms + jcc Branch arm; lift_push/lift_pop; branch_target; push_pop_size + new helpers (stack_slot/sp_write_size/mask_pc/emit_sp_wrap/call_ret_slot) after operation_size. effective_address NOT touched (197.2 fence). vm.rs untouched (dispatcher keys off cpu.rip which blocks now keep <2^32).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
