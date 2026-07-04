# Open backlog — remaining work

The milestone files (`m0`…`m8`, `integration-native-diff`) are **closed**: their
delivered scope is done. Everything still open was moved here so there's one live
list. Items keep their original IDs; new work gets fresh IDs (`DYN-*`).

**Spec:** `wiki/design/spec.md`, `wiki/design/testing.md`. Pick by value, not order.

---

## A. Real programs & OS surface (the forcing function — highest value)

Climb the ladder of real binaries; each surfaces the next real gap.

- [ ] **INT-T9** — Corpus ladder. **Done:** `sha256sum`/`wc` (real busybox), musl `sha256sum`, **real `sqlite3`** (in-memory query), **real `lua`** (x87 exercised). **Next:** `python -c`, then heavier (`gzip`, real file-DB sqlite). (testing.md §12.5)
- [ ] **INT-T10** *(acceptance)* — file-DB sqlite: `sqlite3 test.db < ops.sql`. The **in-memory** variant (query as argv, `:memory:`) already passes three ways; the file-DB + stdin form needs writable-file passthrough (`open` O_RDWR/O_CREAT, `pwrite`, journal) and a stdin buffer. (testing.md §12.5)
- [ ] **INT-T5** — vDSO: expose a guest-visible vDSO or force `clock_gettime`/`gettimeofday` down the syscall path. (Both are stubbed in the shim to a fixed epoch today.) (testing.md §12)
- [ ] **Syscalls on demand** — extend the shim as programs require. **Covered:** file I/O, `mmap`/`brk`, `stat`/`fstat`, `writev`, `lseek`, `fcntl`, `access`, `clock_gettime`/`gettimeofday`, sig/uid/pid stubs. **Next:** writable-file passthrough, `getrandom`, `mprotect`, `MAP_FIXED`/file-backed `mmap`, sockets (`msghdr`), `clone`/`futex` (threaded guests). (testing.md §12.5)

Instruction gaps the ladder keeps surfacing — **filled so far:** `bt*`, `cpuid`, `bsf`/`bsr`, `cwd`/`cdq`, `pshuflw`/`pshufhw`, `pextrw`, `movhps`/`movlps`/`movlhps`/`movhlps`, and the **x87 FPU** (f64-backed — true 80-bit precision deferred; raw `%Lf` output isn't bit-exact). **Still likely ahead:** `shld`/`shrd`, more SSSE3/SSE4 (`pshufb`, `palignr`, `pextrd`/`pinsrd`), true-80-bit x87. Add each when a live path hits it, validated interp == JIT == Unicorn.

## B. Dynamic linking

Faithful path: map and run the real `ld.so` in-guest (it does the relocations
itself). **musl works end to end** (`ld-musl` is a single self-contained
interpreter = libc); the engine core was untouched — the whole feature is the ELF
loader + the mmap/mprotect shim, confirming the guest/OS boundary (§1).

- [x] **DYN-T1** — Load a `PT_INTERP` `ET_DYN` PIE: `load_dynamic_elf` maps the exe + interpreter at load biases; `setup_stack_dyn` builds the full auxv (`AT_PHDR/PHENT/PHNUM/BASE/ENTRY/PAGESZ/RANDOM/HWCAP/uid-gid`). Enters at the interpreter. (§4, testing.md §12)
- [x] **DYN-T2** — Shim honors `mmap` `MAP_FIXED` (returns the requested address — the flat region is already RW) and no-ops `mprotect`/`munmap`. The loader maps each object's full page span. *File-backed runtime `.so` mmap isn't needed for musl (its interpreter is pre-mapped); glibc will need it — see below.* (§4.1, §9.1)
- [x] **DYN-T3** — TLS (initial-exec): `arch_prctl(FS_BASE)` lands and ld.so sets up the TLS block via the mmap arena; FS-relative accesses work (the musl hello returns cleanly). (§16)
- [x] **DYN-T4** *(acceptance)* — a dynamically-linked musl PIE runs three ways (native == interpreter == JIT), `tests/dynamic.rs`. (testing.md §12.5)
- [ ] **DYN-T5** — **glibc** (in progress, blocked). Groundwork landed (`143faea`): file-backed `mmap`, `MAP_FIXED` bss-zeroing, a suffix redirect for `libc.so.6`, the SSE2 string ops glibc uses (`pmovmskb`, `pminub`/`pmaxub`/`pminsw`/`pmaxsw`, `pcmpgt`), `rdtsc`/`rdsspq`, and the startup syscalls (`pread64`, `newfstatat`, `prlimit64`, `getrandom`, `set_robust_list`, `rseq`, `mprotect`). **State:** `ld-linux` loads, maps `libc.so.6`, applies relocations, and runs — then trips on **symbol-version resolution** (prints `no version information available (required by ...)` to stderr and exits 127; `write(1)` is never resolved). **Next:** trace ld.so's version-table setup — likely a mis-read of libc's `.gnu.version`/`DT_VERSYM` (a subtle mmap-vaddr-skew or a relocation the emulation applies wrong), or a missing auxv entry (`AT_SYSINFO_EHDR`/vDSO). Needs ld.so-level debugging (an `LD_DEBUG=versions`-style comparison against native). Gateway to as-shipped distro binaries.

## C. Deferred / hardware-gated

- [ ] **M7-T4** — `MemConsistency` tiers in codegen: `Fast`=bare LDR/STR, `AcqRel`=STLR/LDAPR, `FullTso`=+`DMB`. No-op on x86 (all tiers identical) → **needs an ARM host** to validate. (§8.2.3, §11)
- [ ] **M7-T4c** — Tier baked per `Vm`; a switch flushes the whole cache (don't key the cache by tier). (§8.2.3)
- [ ] **M8-T4** — MXCSR / vector FP flags (rounding-mode control, exception flags). No program has demanded it; convert-to-int saturates (x86 integer-indefinite deferred). (testing.md §10)
- [ ] **M4-T10** — MMIO / trap in the JIT: MMIO-read resume as a pending value consumed by the retried load (RIP on the faulting insn). No MMIO device consumer yet. (§5.2, §16)

## D. Optional / covered elsewhere

- [ ] **M5-T2** — Lazy flags (Variant B): store last-op + operands, compute a flag only when read. Perf only; materialized flags are correct today. (§3.2)
- [ ] **M5-T3** — Superblocks / traces, if profiling justifies. (§12 M5)
- [ ] **M1-T14b** — `NativeOracle` (x86-host fast path replacing `hlt` with a non-privileged terminator). Optional — Unicorn already provides the truth. (testing.md §2, §4)

---

## Exit

This file drains as items land or are consciously dropped. When a group empties,
delete it. New forcing-function gaps (instructions/syscalls) get logged under **A**
before being fixed (testing.md §6.3).
