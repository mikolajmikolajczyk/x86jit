# Open backlog — remaining work

The milestone files (`m0`…`m8`, `integration-native-diff`) are **closed**: their
delivered scope is done. Everything still open was moved here so there's one live
list. Items keep their original IDs; new work gets fresh IDs (`DYN-*`).

**Spec:** `wiki/design/spec.md`, `wiki/design/testing.md`. Pick by value, not order.

---

## A. Real programs & OS surface (the forcing function — highest value)

Climb the ladder of real binaries; each surfaces the next real gap.

- [ ] **INT-T9** — Corpus ladder. **Done:** `sha256sum`/`wc` (real busybox), a musl `sha256sum`, hello. **Next:** `sqlite3` (`test.db` + `ops.sql` → row set) → `lua`/`python -c`. Static builds first. (testing.md §12.5)
- [ ] **INT-T10** *(acceptance)* — `sqlite3 test.db < ops.sql`: native == interpreter == JIT, byte-identical result sets + exit codes. (testing.md §12.5)
- [ ] **INT-T5** — vDSO: expose a guest-visible vDSO or force `clock_gettime`/`gettimeofday` down the syscall path. (testing.md §12)
- [ ] **Syscalls on demand** — extend the shim as programs require: `lseek`, `getrandom`, `mprotect`, `MAP_FIXED`/file-backed `mmap`, sockets (`msghdr`), `clone`/`futex` (gated on threaded guests). (testing.md §12.5)

Instruction gaps a bigger binary will likely surface (busybox's *unused* paths already show them): `bsf`/`bsr`, `shld`/`shrd`, more SSSE3/SSE4 (`pshufb`, `palignr`, `pextrd`/`pinsrd`), and the x87 FPU. Add each when a live path hits it, validated interp == JIT == Unicorn.

## B. Dynamic linking (new — big real-world step)

All fixtures are static today. Real-world binaries are dynamically linked. Faithful
path: map and run the real `ld.so` in-guest (it does the relocations itself).

- [ ] **DYN-T1** — Load a `PT_INTERP` ELF: map the main object, map the interpreter (`ld-musl`/`ld-linux`) at a base, and set up a full auxv (`AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_BASE`, `AT_ENTRY`, `AT_PAGESZ`, `AT_RANDOM`, `AT_HWCAP`). Enter at the interpreter's entry. (§4, testing.md §12)
- [ ] **DYN-T2** — `mmap`/`mprotect`/`munmap` faithful enough for the loader: honor `MAP_FIXED`, PROT changes, and file-backed maps of the `.so` (passthrough `openat`+`read`, or map segments). Keep clear of the JIT arena (W^X). (§4.1, §9.1)
- [ ] **DYN-T3** — TLS for the dynamic (initial-exec) model: `arch_prctl(FS_BASE)` already lands; verify TLS-relative accesses and `__tls_get_addr` if used. (§16)
- [ ] **DYN-T4** *(acceptance)* — a dynamically-linked binary (dynamic busybox, or `/bin/echo`) runs three ways (native == interpreter == JIT) with identical output. (testing.md §12.5)

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
