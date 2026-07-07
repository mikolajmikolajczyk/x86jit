---
id: TASK-117
title: 'CR — lock bts/btr/btc [mem],reg lifts to a non-atomic byte RMW'
status: Done
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-07-07 10:22'
labels:
  - 'crate:core'
  - 'goal:fix'
milestone: code-review
dependencies: []
ordinal: 126000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
lift.rs mem-BT has no has_lock_prefix -> AtomicRmw path (matches the immediate-form gap). Concurrent lock bit-ops on a shared bitmap can tear. Pre-existing; relevant once P2 threads land.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed: lock bts/btr/btc [mem],reg|imm now lifts to an atomic RMW. Refactored lift_bt mem branch into emit_mem_bt(ea,esize,bit,op,locked): locked non-Test emits mask=1<<(bit&width-1), AtomicRmw (Or/Xor/And ~mask), CF via Bt Test on old; non-locked keeps Load/Bt/Store. Covers reg-index (byte-string) + imm-index (operand-width). New jit==interp test locked_bit_ops_match_interp; differential + atomics green. (Single-threaded can't observe atomicity; verified result+CF parity across backends.)
<!-- SECTION:NOTES:END -->
