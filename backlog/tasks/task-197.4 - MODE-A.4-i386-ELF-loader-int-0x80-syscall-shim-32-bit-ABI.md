---
id: TASK-197.4
title: 'MODE-A.4: i386 ELF loader + int 0x80 syscall shim (32-bit ABI)'
status: To Do
assignee: []
created_date: '2026-07-10 10:32'
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
- [ ] #1 i386 static hello (musl or glibc) loads and runs to exit 3-way
- [ ] #2 int 0x80 dispatches through the shim with i386 numbers and 32-bit struct translation
- [ ] #3 Non-i386 32-bit ELFs still rejected loudly (spec §17.7)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
