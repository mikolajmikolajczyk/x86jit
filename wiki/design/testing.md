# Testing architecture: `x86jit`

Companion document to `spec.md` (v0.3). Version 0.1.

Defines how to generate test inputs/outputs, how to add a test every time you find a new edge case, and how to test optimizations (correctness AND whether they actually work).

---

## 0. Overriding principle: the oracle problem

Testing an emulator comes down to one question: **how do you know what the CORRECT result of an execution is?** Writing expected states by hand (registers + 6 flags after every instruction) is unfeasible and would itself be a source of bugs. The solution: **a trusted source of truth (oracle) computes the result for you**, and you record it once as a permanent test vector.

Three oracles, three roles:

| Oracle | Runs on | Role | Note |
|---|---|---|---|
| **Unicorn** (`unicorn-engine`, wrapped QEMU) | x86 and ARM | **main** — vector generation, differential fuzzing | Cross-platform: survives the move to the M4 Pro |
| **Native** (in-process execution on the host CPU) | x86 only | fast-path on the desktop | Stops working on ARM — non-durable |
| **Interpreter** (your own) | everywhere | oracle **for the JIT** | Only after validation against Unicorn |

> **Why Unicorn, not just native.** The native oracle (write bytes into an executable host buffer, set registers, jump, read back the state) is free and accurate, but executes x86 only on an x86 host. After moving to the M4 Pro (ARM) the native oracle disappears. Unicorn emulates x86 independently of the host, so the same tests run on both machines. Keep native as an optional fast oracle while you're on the desktop; Unicorn as the foundation.

---

## 1. Three levels of testing

1. **Per-instruction / per-block (vectors).** The core. Input state + code → expected output state. Source of truth: the oracle. This is 90% of semantic coverage.
2. **Live differential (fuzzing).** Random programs → oracle vs engine → divergence = new edge case to record as a vector. This *discovers* edge cases.
3. **Whole-program (end-to-end).** A real ELF → check the output/exit code. This catches integration (loader ↔ lift ↔ dispatcher ↔ syscall shim).

---

## 2. The test vector — the central artifact

Everything revolves around the vector: a self-contained package (initial state, code, expected final state) that runs **deterministically on any host, without an oracle**. You generate it once (from an oracle or by hand), after which it is a permanent regression test.

```rust
pub struct TestVector {
    pub name: String,              // unique id, e.g. "add_r32_zeroes_upper"
    pub note: String,              // which edge case it covers (for humans)
    pub tags: Vec<String>,         // categories: ["flags","zero-extend"] — for filtering

    pub cpu_init: CpuSnapshot,     // registers/flags on input
    pub mem_init: Vec<MemChunk>,   // memory on input: code + data
    pub entry: u64,                // starting RIP
    pub run: RunSpec,              // how much to execute

    pub expect: Expectation,       // expected final state
}

pub struct CpuSnapshot {
    pub gpr: [u64; 16],
    pub rip: u64,
    pub flags: Flags,              // from the main spec (§3.2)
    pub fs_base: u64,
    pub gs_base: u64,
    // pub xmm: [u128; 16],        // added together with SIMD (M8+)
}

pub struct MemChunk {
    pub addr: u64,
    pub bytes: Vec<u8>,
    pub kind: RegionKind,          // Ram | Trap (from spec §4.2)
}

pub enum RunSpec {
    /// Execute exactly N blocks, then compare state.
    Blocks(u64),
    /// Execute until the engine returns Exit (e.g. an Hlt ending a test snippet).
    UntilExit,
}

pub struct Expectation {
    pub cpu: CpuSnapshot,          // expected CPU state
    pub mem_diff: Vec<MemChunk>,   // ONLY the regions that changed (not all of memory)
    pub exit: ExpectedExit,        // how execution ended
}

pub enum ExpectedExit {
    Halted,                        // the snippet ended with Hlt (typical for unit vectors)
    BudgetDone,                    // RunSpec::Blocks(N) executed without a trap-out
    Exit(ExitKind),                // a specific Exit: Syscall, UnmappedMemory{addr}, MmioRead{addr}, ...
}
```

> **Unit snippet convention:** end per-instruction vectors with an `hlt` instruction (or a known marker), so that execution has a clear end and `ExpectedExit::Halted`. This makes "execute this one instruction and stop" unambiguous. For multi-block vectors use `RunSpec::Blocks(N)`.
>
> **Caveat — `hlt` is privileged and the native oracle runs in user mode.** Unicorn and the interpreter handle `hlt` fine, but the `NativeOracle` (§4) executes the snippet *in-process* on the host CPU, where `hlt` faults (`SIGSEGV`/`#GP`). So the native oracle needs a **different terminator**: an `int3` + a `SIGTRAP` handler that snapshots and returns, or a `ret` into a trampoline that captures state. Keep the `hlt` marker for the vector's logical end (interpreter/Unicorn honor it); the native oracle substitutes its own terminator when it assembles the snippet.

> **`mem_diff`, not all of memory:** the expectation records only the regions that changed. The comparator (§5) checks that exactly those and only those changed. This is concise and catches "an instruction wrote somewhere it shouldn't have".

---

## 3. Serialization and directory layout

Vectors live as files — readable, git-diffable, hand-editable.

**Format: RON** (Rust Object Notation) — maps 1:1 onto Rust types, supports nesting (unlike TOML), readable (unlike bincode). Code/memory bytes as a hex string. (JSON would also work; RON is more convenient for Rust structures.)

```
x86jit-tests/
├── src/
│   ├── vector.rs        # TestVector + (de)serialization
│   ├── oracle/          # UnicornOracle, NativeOracle, InterpreterOracle
│   ├── compare.rs       # precise state comparator (§5)
│   ├── capture.rs       # vector generation from an oracle (§6)
│   ├── fuzz.rs          # differential fuzzing + shrinking (§7)
│   └── bin/
│       ├── capture.rs   # CLI: snippet → vector
│       └── fuzz.rs      # CLI: fuzzing loop
├── vectors/             # CORPUS — permanent regression tests
│   ├── flags/           # category: flag computation
│   │   ├── add_cf_carryout.ron
│   │   ├── add_of_signed_overflow.ron
│   │   └── ...
│   ├── zero_extend/     # category: zeroing the upper 32 bits
│   ├── addressing/      # addressing modes
│   ├── shifts/          # shifts/rotations
│   ├── divide/          # division, including #DE
│   ├── found/           # edge cases found by the fuzzer/in the field (auto-added)
│   └── ...
└── programs/            # whole-program category: static ELFs
    ├── hello_static.elf
    └── ...
```

> **The `found/` category** is a bag for edge cases detected by the fuzzer or during debugging of a real program. It realizes the postulate "easy to add a test when you find a new edge case" — it lands here automatically and becomes permanent.

---

## 4. The oracle abstraction

```rust
pub struct VectorInput {                 // what you feed into execution (without expectations)
    pub cpu_init: CpuSnapshot,
    pub mem_init: Vec<MemChunk>,
    pub entry: u64,
    pub run: RunSpec,
}

pub struct RunOutcome {                   // what comes out of execution
    pub cpu: CpuSnapshot,
    pub mem: Vec<MemChunk>,               // memory state after (full, or only changed pages)
    pub exit: ExitKind,
}

pub trait Oracle {
    fn run(&self, input: &VectorInput) -> RunOutcome;
    fn name(&self) -> &str;               // "unicorn" / "native" / "interpreter"
}
```

**`UnicornOracle`** — maps `CpuSnapshot`→Unicorn registers, `mem_init`→Unicorn mappings, executes, reads back. Note: map the FS/GS base registers (Unicorn has `UC_X86_REG_FS_BASE`/`GS_BASE`), because of TLS. Handle stopping: stop Unicorn at `hlt` via a hook, or execute an exactly known number of instructions.

**`NativeOracle`** (optional, x86-host-only) — write `mem_init` into host buffers (code into an executable one), set registers via an assembly prologue, jump, capture the state in an epilogue. Fastest, zero dependencies, but non-durable (disappears on ARM). Keep it behind `#[cfg(target_arch = "x86_64")]`. Note the `hlt` caveat (§2): it runs in user mode, so it must substitute a non-privileged terminator (`int3`+handler / `ret` trampoline) for the vector's `hlt` marker.

**`InterpreterOracle`** — your interpreter wrapped in `Oracle`. Used as the oracle for the JIT (§8) and as the "engine under test" against Unicorn.

**The engine under test** also implements this same execution shape, so differential = `oracle.run(input)` vs `engine.run(input)`, the same result type, the same comparator.

---

## 5. State comparator — precision is key

A test that only says "the states differ" is useless for debugging. The comparator must pinpoint **exactly** what diverged.

```rust
pub struct Divergence {
    pub reg_diffs:  Vec<(RegName, u64 /*expected*/, u64 /*got*/)>,
    pub flag_diffs: Vec<(FlagName, bool, bool)>,
    pub mem_diffs:  Vec<(u64 /*addr*/, u8 /*expected*/, u8 /*got*/)>,
    pub exit_diff:  Option<(ExitKind, ExitKind)>,
}

pub fn compare(expected: &RunOutcome, got: &RunOutcome) -> Option<Divergence>;
// None = match. Some(d) = report with the exact differences.
```

Example report on a zero-extension bug:
```
FAIL add_r32_zeroes_upper
  reg RAX: expected 0x0000_0000_0000_0002  got 0xFFFF_FFFF_0000_0002
  flag OF: expected false  got true
```
Right away you know you forgot to zero the upper 32 bits and computed OF wrong. This is the difference between a test that *diagnoses* and a test that only *alarms*.

> **Note on undefined flags:** some x86 instructions leave part of the flags in an *undefined* state (architecturally undefined — e.g. AF after some operations, flags after `mul`). The oracle (Unicorn/native) will give *some* value, but you don't have to replicate it, because real code doesn't rely on it. The vector should be able to **mask** the undefined flags for a given instruction (a `dont_care_flags` field or a per-vector list of ignored bits), otherwise you're chasing a match that doesn't matter. This is a real pitfall: without a mask, differential fuzzing will flood you with "divergences" on undefined flags.

---

## 6. The "add a test when you find an edge case" mechanism

This is the heart of the postulate. Two paths — CLI (for generated ones) and inline (for manual ones).

### 6.1 CLI capture — snippet → vector from an oracle

```
$ cargo run -p x86jit-tests --bin capture -- \
    --asm "add eax, ebx; hlt" \
    --init "rax=0xFFFFFFFF00000001, rbx=2" \
    --name add_r32_zeroes_upper \
    --tags flags,zero-extend \
    --note "writing to eax zeroes the upper 32 bits of rax; also checks OF" \
    --out vectors/zero_extend/
```

What it does: (1) assembles the snippet (iced encoder / code assembler), (2) builds a `VectorInput` from the `--init` state, (3) runs it through **Unicorn** (the oracle), (4) captures the result as an `Expectation`, (5) writes the `.ron`. From this moment it is a permanent test. **This is the command you run every time you hit a new edge case.**

### 6.2 Inline builder — for hand-crafted cases

```rust
#[test]
fn add_r32_zeroes_upper() {
    Vector::asm("add eax, ebx; hlt")
        .init(|c| { c.gpr[RAX] = 0xFFFF_FFFF_0000_0001; c.gpr[RBX] = 2; })
        .tag("flags").tag("zero-extend")
        // two expectation modes:
        .expect_via_oracle()               // the oracle computes the result (on a machine with Unicorn)
        // .expect(|c| c.gpr[RAX] == 2)    // or a manual assertion (when you know the result / no oracle)
        .run_on::<Interpreter>();          // and/or ::<Jit>()
}
```

`expect_via_oracle()` computes the expectation with Unicorn during the test; `expect(...)` lets you write the expectation by hand (useful when you want to document a specific result or when you're testing something the oracle doesn't cover). You can also run both backends in one test and require that they produce the same thing.

### 6.3 The rule: every bug = a vector, BEFORE you fix it

Workflow when you find a bug (in the field or from the fuzzer): first record a minimal vector reproducing the bug (into `found/`), confirm that it *fails*, only then fix it, confirm that it *passes*. This guarantees the bug never comes back and that the fix actually works. Classic TDD, but the vector is cheap to create thanks to 6.1.

---

## 7. Differential fuzzing — discovering edge cases

Vectors cover what you anticipated. The fuzzer finds what you didn't.

```rust
loop {
    let program = gen_valid_program(&supported_instrs, &mut rng);  // iced encoder
    let init    = gen_random_state(&mut rng);
    let input   = VectorInput { cpu_init: init, mem_init: with_code(program), .. };

    let oracle_out = unicorn.run(&input);
    let engine_out = engine.run(&input);

    if let Some(div) = compare(&oracle_out, &engine_out) {   // with undefined-flag masking!
        let minimal = shrink(&input, &unicorn, &engine);     // §7.2
        save_vector(&minimal, "vectors/found/");             // auto-add regression
        report(&minimal, &div);
    }
}
```

### 7.1 Generating valid programs

Don't randomize bytes (they'd be mostly garbage/DecodeFault). Randomize **valid instructions from your supported set**, assembling them with the iced encoder (code assembler). You start with the M1 set and expand it together with the lift. For memory operands, map a safe region and constrain addresses to it (otherwise constant UnmappedMemory will dominate the results). Control:
- the instruction distribution (so it isn't 90% `mov`),
- boundary values in registers (0, 1, -1, MAX, values near overflows — not just purely random 64-bit),
- program length (short ones are easier to minimize).

### 7.2 Shrinking (minimization)

A divergence found in a 200-instruction program is useless for debugging. Minimize: remove instructions / simplify operands / zero out registers, as long as the divergence persists. The result: the smallest program that still diverges the engine from the oracle — the ideal vector. This is standard delta-debugging; write it once.

### 7.3 Determinism

A fuzzer with a seed must be reproducible — record the seed on every divergence so it can be reproduced. The engine must be deterministic for a given input (no syscalls/MMIO in the fuzzed programs — see §9).

---

## 8. Testing optimizations — TWO axes

This is where the pitfall lives. An optimization requires *two* independent kinds of test.

### 8.1 The correctness axis: the optimization doesn't change observable behavior

You run the whole corpus of vectors through a **configuration matrix**; each must produce identical state to the baseline (interpreter):

```rust
enum Config {
    Interpreter,              // BASELINE — correctness oracle
    JitNoOpt,
    JitOpt(Opt),             // each optimization SEPARATELY
    JitAllOpts,
}

// for each vector × each configuration:
assert_eq!(compare(&interp_out, &config_out), None);  // everything == baseline
```

Testing each optimization separately (not just "all at once") localizes which optimization broke things when something cracks.

### 8.2 The "did the optimization work AT ALL" axis — the no-op pitfall

**An optimization can break in a way that does nothing — and pass the correctness tests, because if it changes nothing, it breaks nothing.** Example: block chaining that, due to a bug, never stitches blocks — correctness OK (because it behaves as if without chaining), but the optimization effectively isn't there. Correctness tests will NOT detect this.

The solution: **optimization event counters** + targeted tests that the counter ticked.

```rust
pub struct OptStats {
    pub chained_jumps: u64,        // how many jumps were stitched (block chaining)
    pub elided_flag_calcs: u64,    // how many flag computations were skipped (lazy flags)
    pub superblocks_formed: u64,   // ...
    // a counter for each optimization
}

#[test]
fn chaining_actually_fires() {
    // a crafted input: a loop where block A always jumps to B
    let stats = run_with_stats(loop_program, Config::JitOpt(Opt::Chaining));
    assert!(stats.chained_jumps > 0, "chaining didn't work — silent no-op!");
}
```

Each optimization comes with a pair: (a) correctness vectors from §8.1 (doesn't break anything), (b) a targeted test on a crafted input that the counter ticked (it works).

### 8.3 The performance axis (separate — this isn't correctness)

That an optimization *doesn't break* and *fires* doesn't mean it *helps*. A separate benchmark (criterion or your own) measures throughput on representative workloads, opt on vs off.

```rust
// benchmark: the same workload, two configurations, compare time/throughput
bench("workload_x", Config::JitNoOpt);
bench("workload_x", Config::JitOpt(Opt::Chaining));
```

> **The benchmark pitfall:** a micro-benchmark on a single loop misleads. Measure on a realistic mix of blocks (or on a real program from `programs/`), compare *relatively* (on vs off), not in absolute numbers. An optimization may speed up one pattern and slow down another — the benchmark should show the net effect on a realistic workload.

---

## 9. Determinism and syscall stubbing

Vectors must be reproducible. Pure arithmetic is deterministic. But syscalls and MMIO depend on the outside world → non-deterministic.

- **Unit vectors and fuzzed programs:** no syscalls/MMIO. Pure computation. Deterministic by definition.
- **Vectors/tests that MUST contain a syscall:** provide a **scripted responder** — a deterministic table of "on syscall no. X with these args return this". Then trap-out → scripted response → resume is repeatable.

```rust
pub struct ScriptedSyscalls {
    pub responses: Vec<(/*nr*/ u64, /*ret*/ u64, /*effects*/ Vec<MemChunk>)>,
}
```

- **Whole-program tests (`programs/`):** if the program does I/O, use a scripted responder and compare the captured output (e.g. a buffer "written" by `write`) with the expected one. You test "hello world" by checking that the content passed to the stubbed `write` == "hello\n" and the exit code == 0.

---

## 10. Edge-case checklist (coverage categories)

Fuzzing finds the unforeseen; this list covers the foreseeable. Each item = a vector directory. Add to it when you discover a new class.

**Flags (separately for each operation):**
- CF: carry-out / borrow at the size boundary
- OF: signed overflow (positive+positive=negative, etc.)
- ZF: result is zero; SF: sign bit; PF: parity of the low byte; AF: carry out of bit 3
- flags after `mul`/`imul`/`div` (some undefined — mask, §5)
- flags after shifts (CF = last bit shifted out; OF only for a shift by 1)

**Sizes and extension:**
- writing to r32 zeroes the upper 32 bits; r8/r16 preserve them
- sign-extend: `movsx`, `movsxd`, `cdqe`/`cwde`/`cbw`
- zero-extend: `movzx`
- operations on `al`/`ah`/`ax`/`eax`/`rax` — partial registers

**Memory addressing:**
- all combinations of base / index*scale / disp
- RIP-relative (base = the next instruction)
- FS/GS segment override (TLS)
- read-modify-write (`add [mem], reg` — the address computed once)

**Control:**
- every jcc condition (signed vs unsigned — the l/g vs b/a distinction)
- indirect jmp/call (target from a register/memory)
- ret (target from the stack), push/pop (RSP), stack alignment

**Boundary arithmetic:**
- division by zero → #DE → trap-out (how do you represent it? Exit::Fault or a dedicated one)
- division overflow (the quotient doesn't fit)
- shift by 0 (flags untouched), shift by >= size (masking the count through CL)

**Later (M8+):**
- SSE/AVX (XMM/YMM, vector operations, separate MXCSR flags)
- string ops (`rep`, the DF direction)

---

## 11. Integration with milestones (when to build what)

- **M1 (interpreter):** `TestVector`, `compare`, `UnicornOracle`, the `capture` CLI. The first vectors for the M1 set. Differential interpreter-vs-Unicorn run manually. This is the foundation — build the harness together with the interpreter, not after it.
- **M2 (first program):** the `programs/` category, the scripted syscall responder, the "hello world" test.
- **M3 (cache):** hit/miss test (counters), that the loop doesn't re-lift.
- **M4 (JIT):** `InterpreterOracle` as the oracle for the JIT; the configuration matrix (§8.1) — every vector: interpreter == JIT. Run the fuzzer for real (§7).
- **M5 (optimizations):** for each optimization — an `OptStats` counter + a targeted "fires" test (§8.2) + a benchmark (§8.3). The whole corpus through the configuration matrix.
- **M4+ (whole-program differential):** once the JIT runs and syscall passthrough exists, add native-vs-interpreter-vs-JIT comparison on real static binaries (§12). This is the macro integration oracle.
- **Ongoing:** the fuzzer in the background keeps adding vectors to `found/`; every bug in the field → a vector before the fix.

---

## 12. Whole-program differential vs native (syscall passthrough)

Level 3 from §1, taken to its strongest form. Instruction vectors (§2) prove CPU semantics per block; this axis proves the **whole pipeline** end-to-end — loader → lift → dispatcher → cache → backend → syscall layer — by running a real application two ways and comparing its observable output.

The trick: on an x86 host you don't need to emulate the OS. Run the guest's own binary + libraries and **forward each `Exit::Syscall` to the real host kernel** (the qemu-user model). The real world (files, sockets, time) provides the environment; you only marshal arguments. The native run of the same binary is a **free end-to-end oracle**.

### 12.1 Why this exists (why x86-on-x86 is not pointless)

Running recompiled x86 on an x86 host looks redundant — but it's the cheapest, strongest **integration** test you have. Instruction vectors can't catch a loader bug, a cache-invalidation bug, or a syscall-marshalling bug; a whole program does. A live app is also a free semantics fuzzer: SQLite alone exercises overflow, bit-twiddling, and SIMD `memcpy` paths you'd never hand-write.

### 12.2 Three configurations, not two

Split the blame surface by comparing three runs of the same fixed input:

```
native x86        → output_A   (oracle: real CPU + real kernel)
your interpreter  → output_B
your JIT          → output_C
A == B == C  ⇒ confidence
```

- `B != A` → bug in lift / interpreter.
- `C != B` → bug in the JIT.

The native host is the end-to-end oracle, exactly like the native oracle in §4 but at application granularity instead of block granularity. It complements Unicorn (micro: block + state); this is the macro axis (app + output).

### 12.3 Compare DETERMINISTIC output, never raw state

Syscall passthrough injects host nondeterminism: `mmap`/ASLR pointers, `clock_gettime`, PID, `getrandom`, thread scheduling order. So do **not** compare the final memory image or registers. Compare the **application's observable artifact**:

- a SQLite query result set, a `sha256sum` digest, gzip output bytes, stdout, exit code.

Pick programs whose output is a pure function of their input. `sqlite3 test.db < ops.sql` is ideal: same DB + same statements → deterministic rows.

### 12.4 Input determinism is on you

For `A == B == C` to mean anything, the input must be fixed (same DB, same commands) and the output must not depend on ASLR/PID/time. If the app prints time/random into its output, either stub it (a scripted responder, §9) or choose a different program. Passthrough makes this easier than full HLE — you only need the app's *input* pinned, not the whole world scripted.

### 12.5 Candidate ladder (deterministic output, few OS layers)

| App | Why it's good | Needs |
|-----|---------------|-------|
| `sha256sum` / `gzip` | output = pure function of input; zero time/randomness | file syscalls only |
| `sqlite3` CLI | deterministic result sets; heavy arithmetic/memory; static build easy | ~40 syscalls, no threads |
| `lua` / `python -c` (no rand) | lots of code, controllable output | dynamic linker + SSE |
| `coreutils` test suite | ready-made input/output pairs = ready-made corpus | broad syscall mix |

Static builds first (skip the dynamic linker); a program that spawns threads or touches the GPU is a much later target (needs M7 and real `clone`/`futex` passthrough).

> **Passthrough is one specific "user", not core.** Spec §1 says HLE belongs to the embedder. The passthrough syscall layer is a thin embedder (forward to host) rather than a thick one (reimplement the OS). It lives in `x86jit-tests` (or a helper crate), never in `x86jit-core`. Keep it x86-host-only and `#[cfg]`-gated, like the native oracle — it's a test convenience, not a shipped feature.

---

## Summary (one sentence per mechanism)

- **Oracle:** Unicorn (cross-platform, main) computes the truth; the interpreter as the oracle for the JIT; native as a fast-path on x86.
- **Vector:** a self-contained package (state+code+expectation), generated once, reproducible everywhere without an oracle — this is a permanent regression test.
- **Adding edge cases:** the `capture` CLI (snippet → vector from an oracle) or the inline builder; every bug = a vector before the fix.
- **Discovering edge cases:** differential fuzzing (valid random programs, oracle vs engine, shrink, auto-save to `found/`).
- **Optimizations:** the correctness axis (the whole corpus == baseline-interpreter, each opt separately) + the "fires" axis (counters, to detect a silent no-op) + the performance axis (benchmark on-vs-off on a realistic workload).
- **Determinism:** pure arithmetic in the vectors; a scripted responder for syscalls/MMIO.
- **Whole-program (§12):** run a real static binary native vs interpreter vs JIT with syscall passthrough on an x86 host; compare deterministic output (SQLite rows, digests) — the macro integration oracle.
