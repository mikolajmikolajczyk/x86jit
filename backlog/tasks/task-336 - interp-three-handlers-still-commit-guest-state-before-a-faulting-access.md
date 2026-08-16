---
id: TASK-336
title: 'interp: three handlers still commit guest state before a faulting access'
status: To Do
assignee: []
created_date: '2026-08-15 13:24'
labels:
  - m8-simd
dependencies: []
priority: medium
ordinal: 372000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The fault-atomicity sweep walked every interp function with two or more textual `vload`s. These three have exactly ONE `vload`, inside a `for` over 128-bit lanes, and write `cpu.xmm[dst]` in the loop body — so the low half is committed before the high half's load can fault.

interp/vector.rs:1176 `exec_v_dpps_m` and interp/vector.rs:3495 `exec_v_psign_m`. With `dst == src1` the lift emits no VMov256, so a retry re-reads its own output:

    vdpps ymm0, ymm0, [m]     exit UnmappedMemory{addr:65536}, xmm0 lo already 0x41000000...
                              after retry lo = 64.0; folded once = 8.0, twice = 64.0
    vpsignb ymm0, ymm0, [m]   exit UnmappedMemory{addr:65536}, xmm0 lo already 0xffff..ff
                              after retry lo = 0x0101..01  (negated twice -> back to +1)

The JIT does NOT share this: emit_v_dpps_m / emit_v_psign_m (codegen/vector.rs:619, 3029) issue one `checked_addr(base, bytes)` covering all 32 bytes before any store_lane. So this is interp-only AND a jit != interp divergence.

interp/vector.rs:112 `exec_v_store_wide` is the store-side sibling: a faulting 32-byte store commits its low half where the JIT commits nothing.

    vmovdqu [rax], ymm0 straddling the end of the span
    interp: UnmappedMemory{addr:65536}, low16 = [ff x16]
    jit   : UnmappedMemory{addr:65536}, low16 = [00 x16]

`exec_v_extract_lane_wide_m` pre-probes the whole destination with `probe_write` for exactly this reason and its doc says 'A real CPU faults the whole store atomically, committing nothing'; `exec_v_store_wide` has no probe.

Blast radius: 13 handlers have a vload/vstore inside a loop; 10 buffer and commit at the end.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 All three compute into a buffer (or pre-probe) and commit only after every faulting access has succeeded
- [ ] #2 A test pins the dst == src1 retry case for dpps and psign, and fails before the fix
- [ ] #3 fault_atomicity.rs covers the VStoreWide store side, which it currently does not
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
