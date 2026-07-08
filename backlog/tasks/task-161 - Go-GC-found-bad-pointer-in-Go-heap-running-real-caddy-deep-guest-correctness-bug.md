---
id: TASK-161
title: >-
  Go GC 'found bad pointer in Go heap' running real caddy (deep
  guest-correctness bug)
status: To Do
assignee: []
created_date: '2026-07-07 17:19'
updated_date: '2026-07-08 04:48'
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
Futex/lock-exclusion probe round (2026-07-08), continuing under the reliable repro:

RULED OUT — futex driver + atomic primitives exclude correctly under load:
- pthreads mutex counter (4 threads x 100k, glibc futex, plain counter++) under 3x nproc oversubscription: 0/30 lost-update failures. Production thread.rs and the mt.rs local driver share near-identical futex_wait/wake (gen-counter under the futex mutex) — no lost/spurious wake.
- contended atomic RMW (lock inc/not/neg — committed) + NEW probe: cas_increment_counter (8 vcpus, lock-cmpxchg CAS-increment loop) and lock_xor_binary_path (8 vcpus, lock xor via lift_binop) both pass. AtomicCas/AtomicRmw are genuinely atomic with correct ZF.
- Lifter atomic coverage verified: lock add/sub/and/or/xor (rmw_of_binop) -> AtomicRmw; xchg-with-memory -> AtomicRmw even WITHOUT a lock prefix (implicit lock, correct); lock cmpxchg -> AtomicCas. No mis-lowered Go atomic found.

FOUND (separate bug, NOT caddy's cause) -> filed TASK-165:
- A plain guest store racing an atomic RMW/CAS on the same location breaks mutual exclusion under interp (as_mut_slice `&mut`-over-backing aliasing UB -> optimizer reorders/elides the store). Deterministic repro: an atomic-cmpxchg-acquire + PLAIN-STORE-release spinlock loses exclusion at 8 vcpus (fails 3/3; atomic-xchg release or lock-inc CS both fix/mask it). The reverted scalar-atomic Memory::read/write fix REPAIRS it (and is zero-cost on x86: Relaxed atomic == plain mov). Full suite 255/255 green with it.
- BUT it does NOT fix caddy: Go's lock release uses atomic xchg (dodges this manifestation). caddy fixed-arm 9/25 (36%) vs baseline 18/40 (45%), Fisher p~0.46 — no effect.

CADDY STILL UNSOLVED. Remaining suspects (per-P/g-pointer angle still open): (a) a plain-store-vs-atomic race on a location Go accesses with BOTH plain and atomic (the publish/subscribe pattern) that TASK-165's fix would also cover but at a site the spinlock doesn't model — worth re-measuring caddy with more samples; (b) a Go-runtime-specific scheduler/allocator corruption not reducible to the mutex primitive; (c) a mis-lifted instruction only on caddy's hot MT path. NEXT: watchpoint the guest mcache/span free-list under the load repro to catch the first overlapping/garbage write.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
