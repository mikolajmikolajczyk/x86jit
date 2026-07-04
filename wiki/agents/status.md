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

## M1 — IR + interpreter + differential harness (complete)

- The pure-Rust execution vertical, unit-tested end to end and matched against Unicorn:
  - Operand lowering (`lift.rs`): single `effective_address` (base+index*scale+disp, RIP-relative via iced, FS/GS segment base), `lower_read`/`lower_write_target`, RMW computes the address once. `CpuMode` seam keeps the literal 64 out of the decoder.
  - `lift_block`: iced decode loop, `InsnStart` per instruction, block ends at control flow. Minimal set lifted: mov, add, sub, cmp, and/or/xor, test, push, pop, lea, jmp, jcc, call, ret, syscall, hlt, nop. High-byte regs (AH/BH/CH/DH) rejected rather than mis-lowered.
  - Interpreter (`interp.rs`): executes IR over a temps vector; Variant-A materialized flags for add/sub/logic; scalar `Memory::read`/`write`/`code_slice` (interior-mutable, bounds-checked). RIP-on-trap convention (faulting insn) and syscall/hlt (past insn), the same rule the JIT will follow. Instruction atomicity baked into push/pop/RMW ordering.
  - **Differential test harness (done, testing.md §2–§6, §11).** RON `TestVector`/`CpuSnapshot`/`MemChunk` (bytes as hex), the `Oracle` trait with `InterpreterOracle` (engine under test) and `UnicornOracle` (cross-platform truth), a precise `compare` with per-vector `dont_care_flags` masking, and a `capture` CLI (snippet → `.ron` via Unicorn). Starter corpus under `x86jit-tests/vectors/` (`flags/`, `zero_extend/`, `addressing/`). Unicorn is behind the `unicorn` cargo feature; it links the nixpkgs `libunicorn` via pkg-config (`dynamic_linkage`, no cmake) with `LIBCLANG_PATH` for the sys crate's bindgen.
  - **Extended instruction set (done).** `adc`/`sbb` (CF-in), `inc`/`dec` (keep CF), `neg`, `not`, `movzx`, `movsx`/`movsxd`, `cdqe`, `cqo`, `setcc`, `cmovcc` (branchless select, always-writes zero-extend). New IR ops `Sar`/`Sext`; `GetCond` drives setcc/cmov. Count-conditional shift flags are still deferred (no guest shift lifted yet — internal address shifts pass an empty flag mask).
  - **Inline builder (`builder.rs`, testing.md §6.2):** `Vector::asm(..).init(..).dont_care(..).assert_matches_unicorn()`; the differential suite routes through it.
  - **Acceptance green:** 20 differential snippets (`--features unicorn`) match Unicorn across the whole M1 set incl. adc/sbb chains, movzx/movsx, setcc, cmovcc; the 7-vector corpus replays on the interpreter with no Unicorn. Default lane 42 tests; unicorn lane 33.
  - **Only optional bit left:** the `NativeOracle` (T14b, x86-host fast path) — deferred; Unicorn already provides the truth.

## M2 — First real program (complete)

- **Emulated code prints "hello world."** A freestanding (nolibc) static x86-64 ELF issuing raw `write`/`exit` runs end-to-end under the interpreter, proving the whole pipeline: loader → lift → dispatcher → interpreter → syscall shim.
  - `x86jit-elf`: static ELF64 loader over `goblin` — checks 64-bit/LE/x86-64, maps each `PT_LOAD` (`p_flags`→`Prot`) with `vm.map`+`vm.write_bytes`, returns `e_entry`. Plus `setup_stack` — builds the System V AMD64 initial stack (argc/argv/envp/auxv, 16-byte-aligned RSP) so a real `_start` finds what it expects.
  - Test-side `LinuxShim` (harness, testing.md §9): reacts to `Exit::Syscall` — `write` (fd 1/2 → captured stdout/stderr, returns count) and `exit`/`exit_group` (records code). `ScriptedSyscalls` fallback for determinism. The core still emulates no OS (§1).
  - Fixture `x86jit-tests/programs/hello_static.{s,elf}` — freestanding, linked at 0x400000, natively runnable (prints `hello`, exit 0). Deliberately NOT static-glibc (that needs SSE2 `memcpy`/`strlen` in `__libc_start_main` → M8).
  - Acceptance: two whole-program tests. `hello_static.elf` asserts stdout == `"hello\n"`, exit 0. `echo_argv.elf` reads `argv[1]`+`argc` off the stack (strlen loop), echoes the arg and exits with argc — proving `setup_stack` semantically (stdout == `"WORLD"`, exit 2), not just by memory inspection. No new instructions were needed (both stay within the M1 set).

## M3 — Translation cache (complete)

- The dispatcher already cloned the `CachedBlock` out of the cache (no lock held across execution — SMC-safe) and lifted on miss. M3 adds `hits()`/`misses()` counters (atomic, `Relaxed`) to `TranslationCache` and an acceptance test: a countdown loop lifts its 3 distinct blocks once each (misses == 3) and re-runs the loop body from the cache (hits grow one-for-one with iterations, misses stay flat). The cache key stays `u64`; the `BlockKey { guest_addr, mode }` seam is a comment only (§17.4).

## M4 — Cranelift JIT (complete bar JIT-side MMIO)

- **A second backend now compiles IR blocks to host code and agrees with the interpreter on everything tested.** The interpreter is the oracle for the JIT (§8).
  - **ABI in the core** (`jit_abi`, shared contract §8.2.1-2): compiled-block signature `fn(*mut CpuState, *mut MemCtx) -> u64`; `CpuOffsets` measured from the `#[repr(C)]` layout (no `offset_of!` MSRV bump); result encoding (`0`=Continue, codes for Syscall/Hlt/Unmapped, fault data in `MemCtx`); `run_compiled` decodes it. `execute()`'s `Compiled` arm calls it.
  - **`x86jit-cranelift`** (`JitBackend` + `codegen`): `JITModule` owns the W^X executable arena (lives with the `Vm`); `materialize(&self)` compiles behind a `Mutex`. `codegen` translates the whole M1 IrOp set — reg read/write with sub-register zeroing, add/adc/sub/sbb/logic with flags computed to match the interpreter bit-for-bit, shifts/sext, `GetCond`, inlined `Load`/`Store` with a **bounds check** (out-of-range → `Exit::UnmappedMemory`, no host UB), and all control-flow terminators.
  - Injected via `Vm::with_backend(cfg, Box::new(JitBackend::new()))` — the core never names the JIT crate.
  - **Acceptance green (config matrix, T16):** JIT == interpreter on the assembled snippet suite (arith/flags/adc-sbb/extend/addressing/branches/setcc/cmov/stack/call-ret/OOB-trap), on the 7-vector corpus, and running both real programs (`hello`, `echo_argv` with argv) end-to-end on the JIT.
  - **Differential fuzzer (done, testing.md §7):** seed-deterministic SplitMix64 generator of random valid programs (arith/logic/adc-sbb/inc-dec/neg-not/mov/movzx-movsx/setcc/cmov/load-store at sizes 1/2/4/8, memory confined to a scratch region), delta-debugging shrinker, auto-save of any divergence to `vectors/found/`. Runs clean: 600 programs JIT-vs-interp (exact) and 300 Unicorn-vs-interp (AF masked) — zero divergence. Measured JIT speedup ≈1.8× on a hot loop (debug, no block chaining yet — M5 unlocks the real wins).
  - **Only deferred:** JIT-side MMIO/Trap + the MMIO-read resume (T10) — the interpreter handles MMIO; the JIT will need it when a device/MMIO workflow exists (none yet). Also a per-page permission bitmap for within-flat unmapped/`#PF` faithfulness (today the JIT bounds-checks the flat buffer — matches the interpreter for mapped and truly-out-of-range access, which is all the tests exercise).

## M5 — Performance: block chaining (done); lazy flags / superblocks pending

- **Block chaining (§12 M5): ~29× JIT speedup on a hot loop** (was ~1.8×). Compiled blocks resolve their direct successor through a per-edge link slot: on a filled slot the block returns `RET_CHAIN` with the next entry and the dispatcher's inner loop jumps straight there, skipping the cache lookup; a cold edge returns `RET_LINK` and the dispatcher fills the slot. Slots are stable `Box<u64>`s owned by the `JitBackend` (baked as constants into the code). Preemption preserved: the budget still ticks per block inside the chain loop, so a tight chained loop (even `jmp self`) yields `BudgetExhausted` (§9.2). Only direct `jmp`/`jcc` edges chain; indirect/ret/call fall back to a normal dispatch.
  - All three M5 axes green (testing.md §8): **correctness** — JIT == interpreter across the suite, the 7-vector corpus, both whole programs, and 600 fuzzed programs; **fires** — a `chained()` counter on the cache asserts the loop back-edge chains (>500 on a 1000-iter loop); **performance** — the ignored `jit_speedup` bench measures the ~29× win.
  - Tail-call chaining (compiled→compiled with no dispatcher touch) was considered but deferred: Cranelift 0.115's `Tail` callconv isn't C-ABI-compatible, so the link-slot + tight-loop design is the safe, portable choice. It already captures the dispatch-overhead win.

## In flight

- Nothing active. Two backends, agreeing everywhere; the JIT is ~29× the interpreter on hot loops. **Remaining M5 (optional, ongoing):** lazy flags (Variant B — compute a flag only when read; §3.2), superblocks/traces if profiling justifies. Or advance to **M6** (SMC invalidation) / **M7** (multithreading + TSO).

## Broken / regressions

- None. Remaining `todo!()`s are milestone markers: the Cranelift/JIT backend (M4, `execute`'s `Compiled` arm; `run_compiled`), `complete_mmio_read` (M2), and `SoftMmu` mapping. They panic if reached, marking the milestone that fills them.

## Not started

Everything past M5-chaining. In milestone order (spec.md §12):

- **M5 tail** (optional) — lazy flags (Variant B), superblocks/traces.
- **M6** — SMC invalidation.
- **M7** — multithreading + TSO.
- **M8+** — SIMD.

See [`deferred.md`](deferred.md) for what's intentionally held back and why.
