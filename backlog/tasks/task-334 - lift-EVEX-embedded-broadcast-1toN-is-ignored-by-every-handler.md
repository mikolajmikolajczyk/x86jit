---
id: TASK-334
title: 'lift: EVEX embedded broadcast {1toN} is ignored by every handler'
status: To Do
assignee: []
created_date: '2026-08-15 13:19'
labels:
  - m8-simd
dependencies: []
priority: high
ordinal: 370000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`insn.is_broadcast()` appears ZERO times in `x86jit-core/src/`. An EVEX load-op with EVEX.b=1 on a memory operand reaches the same handler as the full-width form and emits identical IR, so the broadcast is silently dropped.

SDM Vol 2 §2.7.7, 'Embedded Broadcast Support in EVEX': 'EVEX.b (P[20]) bit is used to enable broadcast on load-op instructions. When enabled, only one element is loaded from memory and broadcasted to all other elements instead of loading the full memory size.'

Two distinct guest-visible failures:

1. WRONG RESULT — lanes 1..N get adjacent memory instead of the replicated element.
2. SPURIOUS TRAP — the engine reads the full operand width, so a {1to4}/{1to8} load in the last dwords of a mapping faults where hardware would not.

Measured, executed through `Vcpu::run`:

    vaddps xmm1, xmm2, dword bcst [rax]     bytes: 62 F1 6C 18 58 08
    memory at 0x2000: [1.0, 100.0, 200.0, 300.0]
    xmm1 = [1.0, 100.0, 200.0, 300.0]       expected [1.0, 1.0, 1.0, 1.0]

    same instruction, source dword at 0x2FFC with 0x3000 unmapped
    exit: UnmappedMemory { addr: 12284, access: Read }   expected Hlt

Extent, swept mechanically (for every Code with `OpCodeInfo::can_broadcast()`, build the memory form with `set_is_broadcast(true)`, RE-DECODE to confirm the bit survived the encoder, lift, diff the IR against the same form without the bit): 360 Codes / 139 mnemonics lift with byte-identical IR, 0 differ, 521 trap.

This is ORTHOGONAL to and larger than the write-mask hole (task-333): 93 of those mnemonics have a handler that honours the write mask correctly and still drops the broadcast — e.g. `vpaddd xmm1{k1}` lifts to `VMaskedPacked{...}`, but `vpaddd xmm1, xmm2, dword bcst [rax+0x40]` lifts to the same `VPackedBinM` as a full 16-byte load.

Example path: `lift_vfloat_bin` -> `IrOp::VFloatBinM` -> `exec_v_float_bin_m` (interp/vector.rs:3752), which hardcodes `let size = if *scalar { prec.bytes() } else { 16 };`.

Not covered by deferred.md or the README known-gaps list, which mention masking only.

Encoder trap for whoever writes the tests: an assembler-built 'broadcast' instruction may not carry the bit — always re-decode and assert `is_broadcast()` before trusting a probe.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The IR carries the broadcast element width, so a broadcast operand loads ONE element and replicates it — both the value and the number of bytes read
- [ ] #2 A memory-fault test pins the read width: a {1to4} load whose element is in the last mapped dword must NOT fault
- [ ] #3 Every mnemonic in the swept list either honours the bit or returns Unsupported; none silently ignores it
- [ ] #4 The compat probe reports broadcast forms honestly, or its blind spot to them is stated in the generated map
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
