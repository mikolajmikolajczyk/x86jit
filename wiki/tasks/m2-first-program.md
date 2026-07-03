# M2 — First real program

**Goal:** the psychological milestone — see "hello world" printed by emulated code. Extend instruction coverage until a static x86-64 Linux ELF runs end-to-end under the interpreter.

**Spec:** spec.md §12 (M2); testing.md §9, §11 (M2). **Prereq:** M1.

## Tasks

- [x] **M2-T1** — Widen instruction coverage as needed to run a real `_start` path (whatever the target hello-world binary touches: more mov/arith variants, `test`, `movzx`/`movsx`/`movsxd`, `cdqe`/`cqo`, `syscall`, stack ops). Add each via the M1 lowering helpers. (§12 M2, §10)
- [x] **M2-T2** — `x86jit-elf` loader: parse `PT_LOAD` segments → `vm.map` + `vm.write_bytes` each; return `e_entry`. Static, x86-64 only. Optional but recommended for the test. (§4.2, §12 M2)
- [x] **M2-T3** — Test-side syscall shim reacting to `Exit::Syscall`: at minimum `write` and `exit` (Linux x86-64: nr in RAX, args in RDI/RSI/RDX). (§5.3, §12 M2)
- [x] **M2-T4** — `ScriptedSyscalls` deterministic responder (nr + args → ret + memory effects) so whole-program tests are reproducible. (T§9)
- [x] **M2-T5** — `programs/` test category; add `hello_static.elf` (checked-in fixture). **Use a nolibc / freestanding binary** (raw `write`/`exit` via `syscall`, `-nostdlib`), NOT a static-glibc one — glibc's `__libc_start_main` calls SSE2 `memcpy`/`strlen` immediately, so a glibc hello secretly needs M8 (SIMD) before it prints. (T§3, T§11 M2, §12 M2, §16)
- [x] **M2-T6** — Set up entry: alloc a RAM stack region, set `Rsp`, `set_reg(Rip, entry)`; drive the `run()` → `Exit` loop from the test harness. (§4.3, §5.3)

## Acceptance

- [x] **M2-T7** — Whole-program test: run `hello_static.elf` under the interpreter; assert the buffer handed to the stubbed `write` == `"hello\n"` (or the binary's exact output) and exit code == 0. (T§9, T§11 M2)

## Exit criteria

Emulated code prints "hello world" through the syscall shim; the full pipeline (loader ↔ lift ↔ dispatcher ↔ syscall shim) works end-to-end. The interpreter is now a usable, if slow, engine.
