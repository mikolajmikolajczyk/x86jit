---
id: TASK-161
title: >-
  Go GC 'found bad pointer in Go heap' running real caddy (deep
  guest-correctness bug)
status: To Do
assignee: []
created_date: '2026-07-07 17:19'
updated_date: '2026-07-07 18:12'
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
Session 2026-07-07 (fresh go1.26.3 caddy rebuild). TWO distinct bugs surfaced; the FIRST is fixed this session, the SECOND is the real remaining caddy blocker and reclassifies this task.

BUG 1 — lifter fetch-window truncation (FIXED). lift_block capped its decode window at 4096B (code_slice(start,4096)). A branchless block longer than 4 KiB truncated its final instruction at the boundary; iced flagged it invalid -> spurious Exit::UnmappedMemory{access:Execute} (looked like a wild jump to a mid-.text addr). Go bignum crypto (crypto/internal/fips140/nistec/fiat.p521Square) has >4 KiB branchless adc/mul stretches -> first trap for the fresh build (after prefetch 0F 18, already fixed). FIX: lift_block detects iced DecoderError::NoMoreBytes at a full (max_len-capped) window and cuts the block cleanly at the last complete instruction, falling through to a continuation block (codegen already supports non-terminated fall-through; flags stay live via elide_dead_flags's all-flags-live-out boundary). Const BLOCK_FETCH_WINDOW. Regression: differential::branchless_block_longer_than_fetch_window (2600x adc chain >5 KiB, interp==unicorn incl carry across the cut). Filed as its own task.

BUG 2 — multi-threaded interpreter memory corruption (REMAINING; this is now what task-161 tracks). With bug 1 fixed, caddy boots the FULL Go runtime + all init. Under JIT: 'caddy version' -> prints 'unknown', exit 0 (SUCCESS through the whole init incl regexp). Under pure InterpreterBackend: panic 'regexp: Compile(<caddy URL regex>): expression too large' (Go regexp size/count overflow -> runtime.throw, exit 2). Likely the SAME corruption class as the earlier build's 'bad pointer in Go heap' (GC scans a garbage pointer; here the regexp parser reads a garbage count).
NARROWING (all cheap, reproduced):
- NOT the regexp code: an isolated Go binary compiling the exact same regex works under interp even with heavy maps/GC/strings/P-521 crypto + 450 diverse compiles. So it's upstream state read by the parser, not the parse arithmetic.
- NOT memory-model/demand-zero: interp and JIT share the SAME Reserved span + memory model; only interp fails.
- NOT a scalar mis-lift and NOT a race in the flaky sense: DETERMINISTIC across runs, BUT vanishes at GOMAXPROCS=1, and vanishes when driven via JitBackend even with every block forced-interpreted (timing perturbation alone hides it). => a MULTI-THREADED interpreter concurrency bug: concurrent guest threads corrupt shared memory; the interpreter's execution timing exposes an ordering/atomicity hole the JIT's timing hides. Suspects: missing TSO store/load ordering on the interp store path (memory.rs write/note_write), a non-atomic RMW/CAS under contention (atomic_rmw/atomic_cas misaligned fallback), or futex wakeup ordering (thread.rs / VCLK decision-6). Ties to MT-correctness tasks 121-125.
REPRO (fixtures local-only, 52 MiB): build via CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go install github.com/caddyserver/caddy/v2/cmd/caddy@latest; strip -> x86jit-tests/programs/caddy.elf. Guest::new_static(CADDY).reserved(1<<40).heap_base(0x600_0000).brk_limit(0x680_0000).mmap_base(0x1_0000_0000).mmap_limit(0x1_0000_0000+(512<<30)).stack_top(0x8000_0000).argv([caddy,version]).run_threaded(InterpreterBackend) -> 'expression too large'. Same with JitBackend OR GOMAXPROCS=1 in env -> exit 0. NEXT: hunt the interp MT ordering/atomicity hole (diff interp vs JIT store+atomic paths; add a stress harness with 2 guest threads racing a shared word).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
