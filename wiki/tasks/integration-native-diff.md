# Integration — whole-program differential vs native

**Goal:** run real static x86-64 binaries through the engine with **syscall passthrough** to the host kernel, and prove correctness by comparing deterministic output against a native run and the interpreter. The macro integration oracle that instruction vectors can't provide.

**Spec:** testing.md §12, §1 (level 3), §9. **Prereq:** M2 (basic whole-program + syscall handling), stronger after M4 (JIT to validate). **Host:** x86-64 only — `#[cfg]`-gated, test-only, like the native oracle. This is a **cross-cutting track**, not a milestone; do it once the pieces exist.

## Why (testing.md §12.1)

Instruction vectors prove CPU semantics per block; they can't catch a loader, cache-invalidation, or syscall-marshalling bug. A live app running native-vs-interpreter-vs-JIT with identical output does. A real app (SQLite) is also a free semantics fuzzer.

## Tasks

### Syscall passthrough layer (thin embedder — lives in `x86jit-tests` or a helper crate, never core)

- [x] **INT-T1** — Syscall dispatch on `Exit::Syscall`: the `LinuxShim` reads nr + args from guest registers, forwards file ops (`open`/`read`/`close`) to the host kernel (via `std::fs`, read-only path allowlist), writes the result to RAX, resumes. x86-host-only in effect. (T§12, §1)
- [x] **INT-T2** — Guest↔host pointer translation for pointer arguments: NUL-terminated path strings, `read`/`write` buffer copies between guest and host, and `writev` iovec-array gathering. (`host_base + guest_addr` is the flat-model translation.) `msghdr`/socket structs deferred (no networking program yet). (T§12)
- [x] **INT-T3** — `mmap` (anonymous bump arena in guest space), `munmap` (no-op), and `brk` place results inside the guest address space. **Deferred:** `mprotect`, `MAP_FIXED`, file-backed `mmap`, and SoftMmu/W^X interaction — not needed by the static flat-model programs run so far. (T§12.3, §4.1, §9.1)
- [x] **INT-T4** — The syscall set a static musl binary needs is covered: `open`/`openat`, `read`, `write`, `writev`, `close`, `stat`/`fstat`, `brk`, `mmap`/`munmap`, `arch_prctl` (FS_BASE), `set_tid_address`, `rt_sigprocmask`, `ioctl`, `get/set uid/gid`, `exit`/`exit_group`. **Deferred until demanded:** `lseek`, `getrandom`. (T§12.5)
- **INT-T5** — moved to [open-backlog.md](open-backlog.md).

### Whole-program differential harness (testing.md §12.2)

- [x] **INT-T6** — The whole-program tests run a fixed-input binary and capture its deterministic output (stdout bytes / exit code), not raw memory/registers. `tests/whole_program.rs`, `tests/busybox.rs`. (T§12.3)
- [x] **INT-T7** — Three-config comparison on the same input: native x86 (spawned process) vs interpreter vs JIT, asserting `A == B == C` — for the freestanding programs, the musl hello, sha256sum, the Newton float program, and real busybox. (T§12.2)
- [x] **INT-T8** — Inputs are pinned (fixed argv + checked-in fixture files); the `ScriptedSyscalls` responder exists for nondeterministic syscalls. No program run so far depends on ASLR/PID/time; an explicit quarantine check is unneeded until one does. (T§12.4)
- **INT-T9** — moved to [open-backlog.md](open-backlog.md).

## Acceptance

- **INT-T10** — moved to [open-backlog.md](open-backlog.md).
- [x] **INT-T11** — Pure-function programs (`sha256sum <file>` — both a musl build and real busybox) match native across all three configs; digest identical. (T§12.5)

## Exit criteria

Real applications run under the engine on an x86 host and their deterministic output matches a native run — end-to-end proof that loader, lift, dispatcher, cache, JIT, and the syscall layer compose correctly. Threaded / GPU apps (`clone`/`futex` passthrough) are a later target gated on M7.
