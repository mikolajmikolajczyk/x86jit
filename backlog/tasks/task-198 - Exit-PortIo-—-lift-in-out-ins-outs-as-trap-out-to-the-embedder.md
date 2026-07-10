---
id: TASK-198
title: 'Exit::PortIo — lift in/out/ins/outs as trap-out to the embedder'
status: In Progress
assignee: []
created_date: '2026-07-10 10:33'
updated_date: '2026-07-10 11:14'
labels:
  - guest-modes
  - machine-exit
dependencies: []
priority: medium
ordinal: 227000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
First piece of the machine Exit surface: port I/O instructions (`in`, `out`, `ins`, `outs`, incl. rep forms) lift to a new `Exit::PortIo { port, size, direction, .. }` instead of Unsupported. The embedder answers reads by writing EAX/AL and resuming — same trap-out shape as MMIO/syscall. Independent of guest modes (works in Long64 today); prerequisite for any machine-style embedder (DOSBox-class, firmware). Cheap and self-contained.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 ins/outs (+rep) either exit per-element or are documented-rejected — decided and tested
- [x] #2 in/out (imm8 and DX forms, sizes 1/2/4) exit with port, size, direction; guest resumes with the embedder-provided value — round-trip integration test with scripted embedder answers, interp and JIT
- [x] #3 A test (mmio_device-style, may double as example) exercises an end-to-end port read/write round-trip
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Impl ce5ca76. Added Exit::PortIo { port:u16, size:u8, dir:PortDir, value:u64 } + PortDir{In,Out}, IrOp::PortIo{port,value,size,dir_out}. Lift: in/out imm8 and dx forms, sizes 1/2/4 (lift_port_io); block terminator that advances RIP past the insn like Syscall. OUT reads al/ax/eax at lift time and carries value; IN resumes via new Vcpu::complete_port_in(value), which writes accumulator through CpuState::write_gpr (central partial-reg path: 32-bit zero-extends, 8/16-bit merge). New pending_port_in:Option<u8> on CpuState records the width. JIT: new RET_PORTIO_DEFER (jit_abi=9); codegen sets RIP to cur_addr and returns it, dispatcher single-steps on interp (mirrors RET_MMIO_DEFER) so interp==JIT with no accumulator plumbing in cranelift.

ins/outs DECISION: documented-rejected as Exit::UnknownInstruction (not lifted). Rationale: no consumer exists (only BIOS-era block-device drivers use rep outsw), and a correct per-element trap-out needs its own restartable-loop machinery (like RepString) — cost not justified without a user. If one surfaces, UnknownInstruction names the exact opcode. Pinned in test (insb/insw/insd/outsb/outsw/outsd/rep-outsw all reject under both backends).

Tests: x86jit-tests/tests/port_io.rs — out round-trip (port/size/value, both encodings+widths), in round-trip w/ sub-register semantics, mmio_device-style port-register round trip (AC#3), ins/outs rejection (AC#1); all under interp AND JIT. Compat map regenerated (+12 lifted, in/out variants). Full suite 346 pass; unicorn suite 300 pass; clippy --all-features and fmt clean. Status left In Progress.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
