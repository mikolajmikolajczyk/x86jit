# Status

Snapshot of what works, what's in flight, what's broken, keyed to the milestones in spec.md §12. **Not the roadmap** — roadmap lives in GitHub issues.

Update this when a milestone advances, a feature lands, or something breaks. Stale status is worse than no status.

## Works

- **M0 — Skeleton (complete).** Cargo workspace with four crates builds clean. Public API types defined across `state`/`memory`/`ir`/`exit`/`cache`/`vm`/`lift`. Dispatcher `run()` loop wired (§9.2). Nix flake devShell verified (toolchain + nextest).
  - Register numbering centralized: `Reg::gpr_index` + `iced_gpr_index` map to `gpr[]` in x86 encoding order, one place (§3.1). `Vcpu::set_reg`/`reg` route through it (rip/fs_base/gs_base handled).
  - `Memory` Flat model live: `map` (tags region prot/kind + bounds/overlap check, no allocation), `write_bytes`/`read_bytes` (mapped-checked), `unmap`.
  - `disasm` module: iced `Decoder` + AT&T `GasFormatter` decode-and-print loop (inspection only, no lift/exec).
  - Acceptance (M0-T10) green: hand-assembled corpus decodes identically to `objdump -d`. 25 tests pass.
- **Spec v0.4 audit applied.** The scaffold reflects the design-class fixes from the implementation audit: guest RAM is interior-mutable (`Memory::write(&self)`, `UnsafeCell` + `unsafe impl Sync`); backend is injected (`Vm::with_backend(Box<dyn Backend>)`, `InterpreterBackend` in core, `JitBackend` stub in cranelift); IR uses `FlagMask` + `Adc`/`Sbb`/`GetCond`; `Exit::Exception` added. See spec.md changelog 0.3→0.4 and §16.

## In flight

- Nothing actively in progress. M0 done — next is M1 (IR + interpreter, minimal instruction set; spec.md §12 M1). Start at `lift_block` / operand lowering in `lift.rs` and the central size-dependent GPR write (M1-T1, the #1 silent bug).

## Broken / regressions

- None. All non-scaffold internals are `todo!()` by design — they panic if reached, marking the milestone that fills them.

## Not started

Everything past M0. In milestone order (spec.md §12):

- **M1** — IR interpreter + minimal instruction set (`mov`/`add`/`sub`/`cmp`/logic/`push`/`pop`/`jmp`/`jcc`/`call`/`ret`/`lea`/load/store), materialized flags, differential tests.
- **M2** — enough coverage to run a static ELF "hello world" under the interpreter; minimal syscall shim in the test harness; optional `x86jit-elf` loader.
- **M3** — translation cache with hit/miss.
- **M4** — Cranelift JIT backend; interpreter as oracle.
- **M5** — perf: block chaining, lazy flags, traces.
- **M6** — SMC invalidation.
- **M7** — multithreading + TSO.
- **M8+** — SIMD.

See [`deferred.md`](deferred.md) for what's intentionally held back and why.
