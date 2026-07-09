---
id: TASK-194
title: Lift FWAIT/WAIT (0x9B) — no-op x87 sync barrier
status: To Do
assignee: []
created_date: '2026-07-09 18:04'
labels:
  - unemups4-migration
dependencies: []
ordinal: 218000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
unemups4 x86jit migration (interpreter backend) hits Exit::UnknownInstruction on opcode 0x9B (FWAIT / WAIT) in the PS4 Orbis CRT __libc_start_main path.

Repro: example ELF examples/ps4-helloworld/hello_world.elf (from unemups4), loaded at guest base 0x400000. Fault RIP 0x403146, byte [9b]. Surrounding disasm (ELF vaddr, +0x400000 at runtime):
  3133: 48 83 c7 08          add    $0x8,%rdi
  3137: 48 8b 32             mov    (%rdx),%rsi
  313a: e8 29 fe ff ff       call   2f68 <__init_libc>
  313f: 48 8d 05 11 00 00 00 lea    0x11(%rip),%rax
  3146: 9b                   fwait          <-- UnknownInstruction here
  3147: 4c 89 f7             mov    %r14,%rdi

Semantics: 0x9B (FWAIT/WAIT) is a single-byte x87 FPU synchronization instruction. On modern x86 with no pending unmasked x87 exceptions and integrated FPU it is effectively a no-op for an interpreter/JIT that does not model x87 exception delivery. Compilers emit it as a benign padding/sync barrier (here between a LEA and a MOV). Treat as no-op (advance RIP by 1). Note: 0x9B is also a legacy prefix for a few x87 instructions (e.g. FSTSW/FCLEX/FINIT via 9B DB/DD...); a standalone 0x9B not followed by an x87 escape (D8-DF) is FWAIT and should decode/execute as a no-op.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 0x9B (standalone FWAIT/WAIT) lifts as a no-op (RIP += 1) in both interpreter and cranelift backends; differential test vs Unicorn covers a standalone FWAIT
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
