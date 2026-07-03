# Integration — whole-program differential vs native

**Goal:** run real static x86-64 binaries through the engine with **syscall passthrough** to the host kernel, and prove correctness by comparing deterministic output against a native run and the interpreter. The macro integration oracle that instruction vectors can't provide.

**Spec:** testing.md §12, §1 (level 3), §9. **Prereq:** M2 (basic whole-program + syscall handling), stronger after M4 (JIT to validate). **Host:** x86-64 only — `#[cfg]`-gated, test-only, like the native oracle. This is a **cross-cutting track**, not a milestone; do it once the pieces exist.

## Why (testing.md §12.1)

Instruction vectors prove CPU semantics per block; they can't catch a loader, cache-invalidation, or syscall-marshalling bug. A live app running native-vs-interpreter-vs-JIT with identical output does. A real app (SQLite) is also a free semantics fuzzer.

## Tasks

### Syscall passthrough layer (thin embedder — lives in `x86jit-tests` or a helper crate, never core)

- [ ] **INT-T1** — Syscall dispatch on `Exit::Syscall`: read nr + args from guest registers, forward to the real host kernel (raw `syscall`/libc), write the result back to RAX, resume. x86-host-only, `#[cfg(target_arch = "x86_64")]`. (T§12, §1)
- [ ] **INT-T2** — Guest→host pointer translation for every pointer argument (`host_base + guest_addr`), including nested structs (`iovec`, `msghdr`, `readv`/`writev`). Per-syscall marshalling. (T§12)
- [ ] **INT-T3** — `mmap`/`mprotect`/`munmap`/`brk` passthrough that places results inside the guest address space (SoftMmu-managed); honor `MAP_FIXED`; keep clear of the JIT code arena (W^X). (T§12.3, §4.1, §9.1)
- [ ] **INT-T4** — Cover the syscall set a static glibc/musl binary needs: `openat`, `read`, `write`, `close`, `fstat`, `lseek`, `writev`, `brk`, `arch_prctl` (FS_BASE!), `set_tid_address`, `getrandom`, `exit_group`. Extend as programs demand. (T§12.5)
- [ ] **INT-T5** — vDSO handling: either expose a guest-visible vDSO or force `clock_gettime`/`gettimeofday` down the syscall path. (T§12)

### Whole-program differential harness (testing.md §12.2)

- [ ] **INT-T6** — Runner that executes a fixed-input binary and captures its **deterministic output artifact** (stdout, exit code, or a named output file's bytes/digest) — NOT raw memory/registers. (T§12.3)
- [ ] **INT-T7** — Three-config comparison on the same input: `native x86` (oracle) vs `Interpreter` vs `JIT`; assert `A == B == C`. Localize blame: `B != A` = lift/interp bug, `C != B` = JIT bug. (T§12.2)
- [ ] **INT-T8** — Input-determinism guard: pin the input (DB, argv, stdin); reject/quarantine programs whose output depends on ASLR/PID/time unless stubbed via the scripted responder (§9). (T§12.4)
- [ ] **INT-T9** — `programs/` corpus fixtures + expected outputs, climbing the ladder: `sha256sum`/`gzip` → `sqlite3` (`test.db` + `ops.sql` → row set) → `lua`/`python -c`. Static builds first. (T§12.5)

## Acceptance

- **INT-T10** — `sqlite3 test.db < ops.sql`: native, interpreter, and JIT produce byte-identical result sets and exit codes. (T§12.5)
- **INT-T11** — At least one pure-function program (`sha256sum <file>`) matches native across all three configs; digest identical. (T§12.5)

## Exit criteria

Real applications run under the engine on an x86 host and their deterministic output matches a native run — end-to-end proof that loader, lift, dispatcher, cache, JIT, and the syscall layer compose correctly. Threaded / GPU apps (`clone`/`futex` passthrough) are a later target gated on M7.
