---
id: TASK-222
title: >-
  control/syscall: syscall RCX/R11 not set, fnstsw m16 mislifted, string-op
  67h/segment/wrap dropped
status: Done
assignee: []
created_date: '2026-07-12 08:07'
updated_date: '2026-07-12 08:57'
labels:
  - 'crate:core'
  - bug
  - code-review
dependencies: []
ordinal: 251000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable whole-codebase review. Control-flow/syscall bugs, confined to the control.rs family: x86jit-core/src/interp/control.rs, x86jit-cranelift/src/codegen/control.rs, x86jit-core/src/lift/control.rs. (1) HIGH: the SYSCALL instruction does not set RCX = next RIP and R11 = RFLAGS in EITHER engine (interp/control.rs ~164, codegen/control.rs ~143). Real hardware SYSCALL writes RCX<-RIP_next, R11<-RFLAGS; userspace that reads them post-syscall (some libc/JIT sysenter shims) sees stale values. Not listed in backlog/docs/deferred.md. Set both in interp and JIT. (2) HIGH: fnstsw m16 is mislifted (lift/control.rs ~374) — it writes AX and never stores to the m16 memory destination. Fix the memory-form to store the FPU status word to [mem]. (3) MEDIUM: string ops (movs/stos/lods/cmps/scas, lift/control.rs ~128) drop the 67h address-size prefix (should use ECX/ESI/EDI 32-bit), the FS/GS segment override (movs fs:.. must add the segment base), and Compat32 32-bit wrap. Honor 67h, segment override, and 32-bit wrap. Verify each against the interp AND jit paths so they stay identical and match hardware.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 SYSCALL sets RCX=next-RIP and R11=RFLAGS in both interp and JIT
- [ ] #2 fnstsw m16 stores the status word to memory (not just AX)
- [ ] #3 string ops honor 67h address-size, FS/GS segment override, and 32-bit wrap
- [ ] #4 cargo nextest (--features unicorn, minus fuzz_robustness) green; clippy -D warnings + fmt clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
First attempt BLOCKED by too-tight file scope. Load-bearing code is shared, not in control.rs family. REQUIRED files: ir.rs (Syscall/RepString IR nodes), lift/mod.rs (both syscall + int0x80 emit Syscall node at ~1674/~1732; string setup), x87.rs (Fnstsw arm ~524 does write_gpr only, never stores to mem), interp/mod.rs (exec_syscall call site + string_run ~3918 reads gpr live 64-bit no seg/mask), cranelift lib.rs (string_helper sig) + codegen/mod.rs/memory.rs. CRITICAL TRAP: the Syscall IR node is SHARED between real syscall AND i386 int 0x80. Setting RCX=next-RIP/R11=RFLAGS unconditionally CORRUPTS 32-bit guests (int 0x80 passes args in ECX; a write(2) got its buffer ptr clobbered -> wrote code bytes to stdout, regressed i386_hello). Must add a discriminator to the Syscall IR variant (is_syscall_insn bool) set at both lift sites, and only latch RCX/R11 for the true syscall instruction. fnstsw m16: needs x87.rs Fnstsw arm to store sw to addr for the mem form. string 67h/segment/wrap: needs addr_mask+segment fields on RepString threaded into string_run + string_helper.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
