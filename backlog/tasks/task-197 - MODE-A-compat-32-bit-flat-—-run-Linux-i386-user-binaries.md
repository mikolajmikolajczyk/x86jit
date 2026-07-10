---
id: TASK-197
title: 'MODE-A: compat 32-bit flat — run Linux i386 user binaries'
status: To Do
assignee: []
created_date: '2026-07-10 10:31'
updated_date: '2026-07-10 12:19'
labels:
  - guest-modes
dependencies: []
references:
  - backlog/docs/design/spec.md
priority: medium
ordinal: 221000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Stage A of the pragmatic guest-mode plan: 32-bit protected/compat mode with flat segments (base 0 except FS/GS), enough to run Linux i386 user-space binaries 3-way (interp / JIT / unicorn diff).

Why: cheapest real second mode; validates all three spec §17 seams (CpuMode §17.3, BlockKey mode §17.4, effective_address §17.5) against a concrete consumer instead of a guessed abstraction. Groundwork every later mode (real16, full protected, V86) reuses.

Scope fence: NO segmentation beyond FS/GS bases, NO GDT/LDT/limits/rings, NO paging, NO runtime mode switching — Vm is constructed in one mode. Full protected mode (C1: descriptors/limits/exceptions, C2: paging/softmmu, V86) stays deliberately deferred until a machine-embedder consumer exists (spec §17.6). Legacy-only instructions (pusha, bound, into, aam/daa, les/lds, push seg) arrive trap-and-fix like AVX-512, not up front.

Subtasks carry the implementation; this parent is done when a real i386 Linux binary runs 3-way.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A real dynamically-or-statically-linked Linux i386 binary (e.g. Debian /bin/echo or a musl hello) runs to exit under interp and JIT with identical results
- [ ] #2 Unicorn 32-bit differential suite passes on the compat-mode lifter
- [ ] #3 Cache cannot confuse blocks across modes (mode is part of the block key, covered by a test)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
MODE-A status after 197.4 (last subtask): a real STATIC i386 (EM_386) Linux binary runs 3-way (native/interp/JIT) to exit — see tests/i386.rs, programs/hello_i386.{s,elf}. All five subtasks (CpuMode plumbing, 32-bit addressing, EIP/stack widths, differential lane, ELF loader + int-0x80 shim) landed on feat/mode-a; full suite green (454 passed, minus fuzz), clippy/fmt clean.

AC#1 (a real i386 binary runs 3-way): MET for a freestanding static binary. AC#2 (unicorn 32-bit differential): 197.5 lane. AC#3 (mode in block key): 197.1 test.

Precise gaps to a FULL i386 userland (all deliberately deferred per spec 17.6, not MODE-A scope): (1) segment-register loads 'mov %ax,%gs' — needed for glibc/musl i386 TLS, lifter does not handle -> Exit::UnknownInstruction; trap-and-fix when a libc i386 binary is the target. set_thread_area/GsBase is already shimmed. (2) dynamic linking (ld-linux.so.2 / PT_INTERP / file-backed mmap2) — loader is static-only. (3) legacy-only i386 instructions (pusha/popa/into/bound/aam/daa/les/lds/push-seg) — arrive trap-and-fix like AVX-512. (4) no vDSO, no signal delivery, no IVT/IDT (int n != 0x80 -> Exit::Exception only).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
