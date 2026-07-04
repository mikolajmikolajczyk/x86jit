# Open backlog — remaining work

The milestone files (`m0`…`m8`, `integration-native-diff`) are **closed**: their
delivered scope is done. Everything still open was moved here so there's one live
list. Items keep their original IDs; new work gets fresh IDs (`DYN-*`).

**Spec:** `wiki/design/spec.md`, `wiki/design/testing.md`. Pick by value, not order.

---

## A. Real programs & OS surface (the forcing function — highest value)

Climb the ladder of real binaries; each surfaces the next real gap.

- [x] **INT-T9** — Corpus ladder climbed to the interpreter summit: `sha256sum`/`wc` (busybox), musl `sha256sum`, **`sqlite3`** (in-memory query), **`lua`** (x87), and **`CPython 3.13`** (`python3 -S -c`, full bytecode VM). Also dynamically-linked musl + glibc hellos. **Further rungs done:** file-DB sqlite (`tests/sqlite_file.rs`), **gzip/gunzip** (DEFLATE, `tests/gzip.rs`), **libjpeg-turbo `djpeg`** (JPEG decode, real SSE2/SSSE3 codec DSP — `tests/djpeg.rs`). **Still open:** a larger python script (more stdlib), `git`/`perl`/`node`, `cjpeg` (encode).
- [x] **INT-T10** *(acceptance)* — file-DB sqlite: `sqlite3 <db> < ops.sql` runs three ways (`tests/sqlite_file.rs`), creating and mutating a real on-disk database. Added a bounded writable-file passthrough (`allow_write_dir` → `O_RDWR`/`O_CREAT`/`O_TRUNC` under a per-test temp dir; `write`/`pwrite`/`ftruncate`/`fsync`/`unlink`/`lstat` + no-op `chmod`/`chown`), a `stdin` buffer, and the `cbw`/`cwde`/`cdqe` sign-extends. (testing.md §12.5)
- [ ] **INT-T5** — vDSO: expose a guest-visible vDSO or force `clock_gettime`/`gettimeofday` down the syscall path. (Both are stubbed in the shim to a fixed epoch today.) (testing.md §12)
- [ ] **Syscalls on demand** — extend the shim as programs require. **Covered:** read/write file I/O incl. **writable passthrough** (`pwrite`/`ftruncate`/`fsync`/`unlink`) + `stdin`, `mmap`/`brk`, `stat`/`lstat`/`fstat`, `writev`, `lseek`, `fcntl`, `access`, `clock_gettime`/`gettimeofday`, chmod/chown no-ops, sig/uid/pid stubs, `clone`/`futex` (threaded guests). `dup`/`dup2` (busybox gzip dups its input onto fd 0), `readv` (libjpeg). **Next:** `getrandom`, `mprotect` (beyond no-op), sockets (`msghdr`), `pipe`. (testing.md §12.5)

Instruction gaps the ladder keeps surfacing — **filled so far:** `bt*`, `cpuid`, `bsf`/`bsr`, `cwd`/`cdq`, `cbw`/`cwde`/`cdqe`, `pshuflw`/`pshufhw`, `pextrw`, `movhps`/`movlps`/`movlhps`/`movhlps`, the **x86-64-v2 (Jaguar) set** (`pshufb`, `popcnt`, `crc32`, `pextrb`, `pcmpgtq`/`pcmpeqq`), and the **x87 FPU** (f64-backed — true 80-bit precision deferred; raw `%Lf` output isn't bit-exact). **Still likely ahead:** `shld`/`shrd`, the SSE4 tail (`palignr`, `pblendvb`, `ptest`, `pmovsx/zx`, `pmulld`, `pinsrb/d`), true-80-bit x87. Add each when a live path hits it, validated interp == JIT == Unicorn.

## B. Dynamic linking

Faithful path: map and run the real `ld.so` in-guest (it does the relocations
itself). **musl works end to end** (`ld-musl` is a single self-contained
interpreter = libc); the engine core was untouched — the whole feature is the ELF
loader + the mmap/mprotect shim, confirming the guest/OS boundary (§1).

- [x] **DYN-T1** — Load a `PT_INTERP` `ET_DYN` PIE: `load_dynamic_elf` maps the exe + interpreter at load biases; `setup_stack_dyn` builds the full auxv (`AT_PHDR/PHENT/PHNUM/BASE/ENTRY/PAGESZ/RANDOM/HWCAP/uid-gid`). Enters at the interpreter. (§4, testing.md §12)
- [x] **DYN-T2** — Shim honors `mmap` `MAP_FIXED` (returns the requested address — the flat region is already RW) and no-ops `mprotect`/`munmap`. The loader maps each object's full page span. *File-backed runtime `.so` mmap isn't needed for musl (its interpreter is pre-mapped); glibc will need it — see below.* (§4.1, §9.1)
- [x] **DYN-T3** — TLS (initial-exec): `arch_prctl(FS_BASE)` lands and ld.so sets up the TLS block via the mmap arena; FS-relative accesses work (the musl hello returns cleanly). (§16)
- [x] **DYN-T4** *(acceptance)* — a dynamically-linked musl PIE runs three ways (native == interpreter == JIT), `tests/dynamic.rs`. (testing.md §12.5)
- [x] **DYN-T5** — **glibc** (`fa2cbac`). A real dynamically-linked glibc binary runs three ways: `ld-linux` loads, file-backed-mmaps `libc.so.6`, resolves versioned symbols, and starts the program — all guest code. The version-resolution blocker was a chain of three shim bugs (fabricated `st_dev`/`st_ino` colliding with the main map's (0,0) in ld.so's dedup; missing `pslldq`; mmap reading `fd=-1` as a 64-bit value and misclassifying anonymous `MAP_FIXED` bss-zeroing as file-backed → stale `__exit_lock` → futex livelock). Groundwork from `143faea` (file-backed mmap, suffix redirect, SSE2 string ops, startup syscalls) plus a futex handler.
  - **Verified working (not committed — machine-specific store paths): dynamically-linked glibc CPython.** `ld-linux` loads the PIE `python3.13`, then dlopen/file-backed-mmaps `libpython3.13.so` + `libc`/`libm`/`libgcc_s` (multiple shared objects), and runs the interpreter three ways. Needed only `time(2)` and an `fstat` on stdin/stdout/stderr returning a character device (both landed). Gateway to as-shipped distro binaries is proven end to end.

## C. Deferred / hardware-gated

- [x] **M7-T4** — `MemConsistency` tiers in codegen. The tier is plumbed from the `Vm` through `Backend::materialize` into codegen; ordinary guest loads/stores route through `gload`/`gstore`, which emit fences on an aarch64 host (`Fast`=bare LDR/STR, `AcqRel`=fence-after-load + fence-before-store, `FullTso`=fence-after-store too). x86 stays plain (native TSO) so every tier is byte-identical there. Proven on the ARM CI runner by a deterministic codegen test asserting the `DMB ISH` count per tier (`tiers_emit_the_right_aarch64_barriers`) and a lock-free message-passing litmus (`tests/tso.rs`). **Follow-up:** use `LDAPR`/`STLR` (RCpc) for a leaner `AcqRel` than the full-`DMB` mapping; provoking an actual `Fast` reorder on the virtualized ARM runner didn't manifest (reorder rate ≈ nil there). (§8.2.3, §11)
- [x] **M7-T4c** — Tier is baked per `Vm` (from `VmConfig.consistency`) and passed to `materialize`; the cache is **not** keyed by tier. There's no runtime tier-switch API yet, so no flush path is needed; add one (flushing the whole cache) if/when a switch is exposed. (§8.2.3)
- [ ] **M8-T4** — MXCSR / vector FP flags (rounding-mode control, exception flags). No program has demanded it; convert-to-int saturates (x86 integer-indefinite deferred). (testing.md §10)
- [ ] **M4-T10** — MMIO / trap in the JIT: MMIO-read resume as a pending value consumed by the retried load (RIP on the faulting insn). No MMIO device consumer yet. (§5.2, §16)

## D. Optional / covered elsewhere

- [x] **M5-T2** — Lazy flags, done as the **compile-time** form (cheaper than the runtime Variant B sketch): a backward liveness pass in `lift_block` narrows each ALU op's `set_flags` to the flags still live, and since the backends gate the flag *store* by the mask, Cranelift's DCE drops the dead flag computation (parity/AF/OF). Plus a **block-local GPR value cache** in the JIT (write-through, so no trap-flush; invalidated after cpuid/x87/string helpers). Together: SHA-256 JIT 28.5 ms → 18.4 ms (~35% faster, 12.2× over interp). Correct vs Unicorn. Runtime Variant B (defer to read) not needed. (§3.2)
- [ ] **M5-T3** — Superblocks / traces, if profiling justifies. (§12 M5)
- [ ] **M1-T14b** — `NativeOracle` (x86-host fast path replacing `hlt` with a non-privileged terminator). Optional — Unicorn already provides the truth. (testing.md §2, §4)

---

## Exit

This file drains as items land or are consciously dropped. When a group empties,
delete it. New forcing-function gaps (instructions/syscalls) get logged under **A**
before being fixed (testing.md §6.3).
