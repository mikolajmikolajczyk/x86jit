---
id: TASK-161
title: >-
  Go GC 'found bad pointer in Go heap' running real caddy (deep
  guest-correctness bug)
status: In Progress
assignee: []
created_date: '2026-07-07 17:19'
updated_date: '2026-07-08 09:22'
labels:
  - go-caddy
  - 'crate:core'
  - 'goal:fix'
milestone: go-caddy
dependencies:
  - TASK-153
ordinal: 170000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real caddy (task-153) boots the FULL Go runtime under interp — GC background workers, finalizer/cleanup/scavenge goroutines all start — then crashes during GC with: 'fatal error: found bad pointer in Go heap (incorrect use of unsafe or cgo?)' (runtime.throw, exit 2). This is a deep guest-correctness bug on the INTERPRETER (so NOT a JIT codegen bug): a pointer-sized word the GC scans is garbage. httpserve_go.elf (net/http stand-in, same Go runtime) works — caddy's heavier/larger code paths exercise something httpserve doesn't (a mis-lifted instruction that corrupts a pointer, or a memory/mmap/brk inconsistency under GC pressure). Needs bisection: which op/instruction/syscall corrupts the scanned word.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Real caddy's GC no longer reports a bad heap pointer; caddy reaches the file-server serve loop (task-153 AC#1)
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
REPRO: build the fixture — CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go install github.com/caddyserver/caddy/v2/cmd/caddy@latest; strip -> ~52 MiB; put at x86jit-tests/programs/caddy.elf. Probe (uncommitted this session): Guest::new_static(CADDY).reserved(1<<40).heap_base(0x600_0000).brk_limit(0x680_0000).mmap_base(0x1_0000_0000).mmap_limit(0x1_0000_0000+(512<<30)).stack_top(0x8000_0000).argv([caddy, version]).env([HOME=/tmp,XDG_DATA_HOME=/tmp,...]).run_threaded_full(InterpreterBackend). Prints the Go panic (task-129 stderr now surfaces it). NOTE heap_base must clear caddy's RW/BSS which tops ~0x5879400 (~88 MiB). BISECT IDEAS: (1) diff caddy vs httpserve instruction coverage (disasm both, find mnemonics only caddy uses) — a mis-lifted SIMD/atomic/BMI is the prime suspect. (2) Check the Reserved-span demand-zero / guard-page path returns consistent bytes for a GC-scanned arena. (3) Watch for a store that doesn't land (write-through vs a mis-sized WriteReg). Fixture is big (~52 MiB) — build locally, don't commit unless gated. Prefetch (0F 18) already fixed this session (was the first trap, before this).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Session 2026-07-08 (see doc-31 §10). MAJOR REFRAME. Ran caddy on JIT (first time): ~70% fail at baseline, no external load (interp 12/12 clean). Discriminators (high-power, both backends): asyncpreemptoff=1 no change; gcstoptheworld=2 no help (worse); GOMAXPROCS=1/2/unset flat ~60-80% => NOT a GC race, NOT concurrency — GC is only the tripwire. MINIMAL REPRO: rgx.elf (~30-line regexp+GC Go prog, recipe in doc §10.3) corrupts interp in 2s at GOMAXPROCS=1, no load. Consistent fault: UnmappedMemory addr=0/2 Read @rip=0x4b0f8e = (*Regexp).MaxCap nil receiver. REGEXP-PATH-SPECIFIC: controls tree.elf(GC+ptrs), copy.elf(rep movsq), deep.elf(copystack) all CLEAN. Instruction-diff bottomed out (rgx minus clean-union empty at mnemonic; Code-form only Seto_rm8/Shr_rm8_imm8=harmless). Both backends corrupt identically => shared LIFTER bug (common instr in regexp-specific pattern, or block-formation). Bisect: present at eaaf0db => predates GP/sigsegv+task-165/163 (excluded). Probe uses reserve() protect:None => no guard pages, wrong EA silently corrupts. NEXT: value-watch the corrupted *Regexp, or unicorn lockstep. Tooling uncommitted: tests/caddy_probe.rs + Guest::build_parts.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
