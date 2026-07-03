# M4 — Cranelift JIT — ✅ working library reached here

**Goal:** compile IR blocks to host code via Cranelift; RAM access inlined, syscalls/Trap trap out. Validate the JIT against the interpreter oracle on the whole corpus, then run the differential fuzzer for real. At the end of this milestone the library is **usable and trustworthy**.

**Spec:** spec.md §8.2, §9.1, §12 (M4), §16; testing.md §7, §8.1, §11 (M4). **Prereq:** M3. **Build incrementally against the interpreter oracle — never write the whole backend at once.**

## ABI / arena — open the black box BEFORE codegen (§8.2.1)

- [x] **M4-T1** — Stable `CpuState` field offsets for codegen (`#[repr(C)]` + `offset_of!` or const table); document as a contract. (§8.2.1, §16)
- [x] **M4-T2** — `MemCtx` (guest buffer `host_base` + metadata) and the compiled-block signature `unsafe extern "C" fn(*mut CpuState, *mut MemCtx) -> u64`. (§8.2.1)
- [x] **M4-T3** — `u64` result encoding: `0` = Continue, non-zero encodes the `Exit` variant (discriminator + data, or details written into CpuState/MemCtx). One place, shared by codegen and `run_compiled`. (§8.2.2)
- [x] **M4-T4** — Executable code arena (`memmap2`, W^X; macOS `pthread_jit_write_protect`), owned by `Vm`, lifetime ≥ cache. `CompiledPtr` borrows into it. (§9.1)
- [x] **M4-T5** — `CompiledPtr: Send + Sync` (already defined in M3/§9.1).

## Codegen — incremental (§8.2.3)

- [x] **M4-T6** — First: an empty block that just writes a new RIP and returns "Continue". Prove the dispatcher jumps in and returns. (§8.2.3 build order)
- [x] **M4-T7** — `Backend::materialize` Jit arm: build a Cranelift `FunctionBuilder`, translate `IrOp`s to a `Temp → cranelift Value` map (`Vec` sized `temp_count`), finalize into the arena → `CompiledPtr`. (§8.2.3)
- [x] **M4-T8** — `run_compiled` decodes the `u64` back into `StepResult`; wire `execute()` compiled arm. (§8, §8.2.2)
- [x] **M4-T9** — Translate `IrOp`s one at a time, validating each against the interpreter: `InsnStart` (bake `guest_addr` as a const for the trapping accesses that follow → store to `cpu.rip` before an `Exit`), `ReadReg`/`WriteReg` (with upper-32 zeroing!), arithmetic/logic, flags in codegen (flag fields at stable `#[repr(C)]` offsets), `Load`/`Store` inlined (`host_base + guest_addr`, no callback), control-flow terminators. (§8.2.1, §8.2.3, §16)
- [x] **M4-T9b** — **Memory-safety strategy for inlined access (zero-th-class decision, §8.2.3).** Raw `host_base + guest_addr` with no check is host UB on any out-of-range guest address. Emit a bounds+permission check (recommended: a predictable branch to a slow-path stub returning `Exit::UnmappedMemory`/`MmioRead`/`MmioWrite`) — the *same* check routes Trap/MMIO out, so it does double duty with M4-T10. Guard pages are a later perf option. In `Flat`, addr 0 is valid → faithful null-`#PF` needs a per-page permission bitmap. (§8.2.3, §16)
- [ ] **M4-T10** — MMIO / Trap in the JIT: fold into the M4-T9b check; implement the **MMIO-read resume as a pending value consumed by the retried load** (RIP on the faulting insn), not a write into a dead temp — works identically in interp and JIT. (§5.2, §16)
- [x] **M4-T10c** — Inject the JIT: `x86jit-cranelift::JitBackend` implements the core `Backend` trait; the user builds the `Vm` via `Vm::with_backend(cfg, Box::new(JitBackend::new(..)))`. The core never names the JIT crate. `materialize(&self)` → compiler state behind a `Mutex`. (§4.1, §8)

## Test tasks (T§11 M4)

- [x] **M4-T11** — `InterpreterOracle` wrapping the interpreter as the oracle for the JIT. (T§4, T§8)
- [x] **M4-T12** — Config matrix (`Interpreter` = base, `JitNoOpt`): every corpus vector must give identical state, JIT == interpreter. (T§8.1)
- [x] **M4-T13** — Differential fuzzer for real: `gen_valid_program` from the supported set (iced encoder, controlled distribution + boundary reg values, memory ops confined to a mapped safe region), oracle vs engine, undefined-flag masking. (T§7, T§7.1)
- [x] **M4-T14** — Shrinking (delta-debugging) of any divergence to a minimal program. (T§7.2)
- [x] **M4-T15** — Seed-determinism: record the seed on every divergence; auto-save the shrunk vector to `vectors/found/`. (T§7.3, T§3)

## Acceptance

- [x] **M4-T16** — JIT == interpreter state on the entire corpus (config matrix green). (§12 M4, T§8.1)
- [x] **M4-T17** — Fuzzer runs clean (or every divergence it finds is captured to `found/`, fixed test-first, and re-passes). Measured JIT speedup over the interpreter. (§12 M4, T§7)

## Exit criteria

Two backends behind one frontend, agreeing on every vector and surviving the fuzzer. RAM is inlined; only syscalls/MMIO trap out. **This is the working library** — everything past here is optimization and reach.
