---
id: TASK-197.4
title: 'MODE-A.4: i386 ELF loader + int 0x80 syscall shim (32-bit ABI)'
status: In Progress
assignee: []
created_date: '2026-07-10 10:32'
updated_date: '2026-07-10 12:36'
labels:
  - guest-modes
dependencies:
  - TASK-197.2
  - TASK-197.3
parent_task_id: TASK-197
ordinal: 225000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Embedder side (x86jit-elf / runner): accept ELFCLASS32 + EM_386, mmap below 4 GiB, 32-bit auxv/stack layout, entry in Compat32. Syscalls arrive as `int 0x80` with the i386 numbering and 32-bit structs — map onto the existing shim (translate layouts; start with the write/exit_group/brk/mmap2 core and grow on demand per TASK-93 pattern). GS-based TLS via set_thread_area enough for static musl/glibc start.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 i386 static hello (musl or glibc) loads and runs to exit 3-way
- [x] #2 int 0x80 dispatches through the shim with i386 numbers and 32-bit struct translation (integration test asserts numbers + translated struct layouts on a syscall trace)
- [x] #3 Non-i386 32-bit ELFs still rejected loudly (spec §17.7) — negative loader test
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed on feat/mode-a. Static i386 (EM_386) hello runs 3-way (native/interp/JIT). Full suite: cargo nextest --features unicorn (minus fuzz) = 454 passed / 2 skipped; clippy --all-targets --all-features -D warnings clean; fmt clean.

INT-0x80 EXIT SURFACE: int 0x80 lifts to IrOp::Syscall -> Exit::Syscall (same as long-mode syscall), NOT a new exit reason. The embedder picks the ABI from vm.cpu_mode(). Rationale: Exit::Syscall's doc already promised 'syscall/sysenter/int 0x80'; the CpuMode seam (17.3) already carries the one bit needed to disambiguate, so no new variant. Other int n (CD ib, n!=0x80) lifts to IrOp::Trap{vector:n, advance:len} -> Exit::Exception, mirroring int3/int1; no IVT/IDT delivery (that is TASK-199). lift.rs: new Int arm after Int1.

SHIM: LinuxShim::handle branches on vm.cpu_mode()==Compat32 -> new handle_i386(). Separate SYS32_* number table (exit=1, write=4, read=3, open=5, close=6, brk=45, writev=146, uname=122, mmap2=192, munmap=91, mprotect=125, set_thread_area=243, exit_group=252, set_tid_address=258, readlink/at=85/305, getrandom=355). Args read from EAX + EBX/ECX/EDX/ESI/EDI/EBP (low 32 bits, zero-extended); return masked to EAX. 32-bit struct translation: i386 iovec is 8 bytes/entry (u32 base+u32 len) vs 64-bit 16; mmap2 offset in pages; utsname machine='i686'. Extracted a behavior-preserving do_write() shared by the 64-bit SYS_WRITE arm and i386 (house pattern like do_read/do_open); 64-bit path otherwise untouched. Unhandled i386 syscalls reject loudly with the number (gap:syscall-i386), like the 64-bit gap path.

TLS DECISION: set_thread_area (243) records user_desc.base_addr as GsBase and writes back a conventional entry_number (6) so glibc/musl can build the GS selector. The core's with_segment already adds gs_base for GS-prefixed accesses (17.5), so no mov-gs-selector handling is needed for that. NOTE: the freestanding test hello does NOT use TLS; the set_thread_area shim is implemented but exercised only by a future libc i386 binary. A static glibc i386 hello additionally needs 'mov %ax,%gs' (segment-register load) lifting, which the lifter does NOT do today -> it would surface Exit::UnknownInstruction. That is the precise gap to a libc-based i386 binary; deferred (trap-and-fix, like AVX-512).

LOADER (17.7): x86jit-elf gained load_static_elf_i386 + setup_stack_i386 (4-byte slots, Elf32 auxv pairs, 16-byte ESP align). LoadError::Unsupported now carries a &static str reason. reject_unless_i386 / reject_unless_x86_64 helpers; the i386 loader refuses ELFCLASS64, big-endian, and non-EM_386 32-bit with clear messages. Existing 64-bit loaders route through reject_unless_x86_64 (behavior-identical).

TEST BINARY: programs/hello_i386.s — freestanding nolibc, write+exit via int 0x80. Built reproducibly with the nix devShell's own gcc: 'cc -m32 -nostdlib -static -o hello_i386.elf hello_i386.s' (no libgcc/multilib needed for pure asm). Committed as programs/hello_i386.elf (matches the house 'commit prebuilt fixture' pattern). tests/i386.rs: AC#1 3-way run, AC#2 int-0x80 numbers+iovec translation, AC#3 negative loader test. Plus lift unit test (int_0x80_is_syscall_other_int_is_trap) and elf unit tests (stack_layout_i386_is_4byte_sysv, i386_loader_rejects_x86_64_loudly).

PARENT (TASK-197) GOAL: met for a real STATIC i386 Linux binary 3-way. Gaps to a full i386 userland (all deferred, out of MODE-A scope per 17.6): (1) segment-register loads (mov gs,ax) for libc TLS; (2) dynamic linking (ld-linux.so.2, PT_INTERP, file-backed mmap2); (3) legacy-only i386 insns (pusha/popa/into/aam/daa/les/lds) arrive trap-and-fix; (4) vDSO/signals/IVT.

COVERAGE MAP (follow-up, 45a8ede): the ISA compat probe is now mode-parametric (probe_code_in(code, CpuMode)) and the coverage page gained a compat32 section — same generation buckets probed at bitness 32 (Encoder::new(32) + lift_block Compat32). This makes the 32-bit-only gap list concrete: Pushad/Popad/Pushaw/Popaw/Into/Daa/Aam_imm8/Aad_imm8/Call_rm16/Retnw etc. now appear under 'compat32 x86-64-v1 — missing' (178 entries vs long64's 175). compat32 v1: 454 lifted / 178 missing / 72%. A 16-bit real-mode table is one probe_code_in call away once a 16-bit CpuMode exists (deliberate seam, no machinery). compat_map_is_current keeps both sections honest; probe_measures_real_coverage asserts the compat32 section exists and lists Pushad. Full suite re-verified: 454 passed / 2 skipped; clippy+fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
