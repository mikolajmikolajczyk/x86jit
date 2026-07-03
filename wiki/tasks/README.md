# Tasks

Ordered work backlog to reach a **working `x86jit` library**, extracted from [`../design/spec.md`](../design/spec.md) (the design) and [`../design/testing.md`](../design/testing.md) (the test architecture). One file per milestone; do them **in order** — each depends on the one before.

## What "working library" means

The minimum viable, correct engine is reached at the **end of [M4](m4-jit-cranelift.md)**: interpreter + Cranelift JIT, translation cache, both validated against the Unicorn oracle and differential fuzzer. [M0](m0-skeleton.md)–[M2](m2-first-program.md) get you an interpreter that runs a real static ELF; [M3](m3-translation-cache.md) adds caching; [M4](m4-jit-cranelift.md) adds the JIT and makes it fast + trustworthy. [M5](m5-performance.md)+ are optimization and reach (SMC, threads, SIMD) — not required for "working".

## Files

| Milestone | Goal | Working library? |
|-----------|------|------------------|
| [m0-skeleton.md](m0-skeleton.md) | Workspace, state, flat memory, decode-and-print | building block |
| [m1-ir-interpreter.md](m1-ir-interpreter.md) | IR interpreter, minimal instruction set, **test harness** | building block |
| [m2-first-program.md](m2-first-program.md) | Run a static ELF "hello world" under the interpreter | building block |
| [m3-translation-cache.md](m3-translation-cache.md) | Cache keyed by guest address, hit/miss | building block |
| [m4-jit-cranelift.md](m4-jit-cranelift.md) | Cranelift JIT + serious fuzzing | **✅ MVP reached** |
| [m5-performance.md](m5-performance.md) | Block chaining, lazy flags, benchmarks | optimization |
| [m6-smc.md](m6-smc.md) | Self-modifying-code invalidation | reach |
| [m7-multithreading-tso.md](m7-multithreading-tso.md) | Multiple vcpus, TSO barriers | reach |
| [m8-simd.md](m8-simd.md) | SSE/AVX, string ops | reach |

**Cross-cutting track** (not a milestone — starts once its prereqs exist):

| Track | Goal | Prereq |
|-------|------|--------|
| [integration-native-diff.md](integration-native-diff.md) | Run real static binaries via syscall passthrough; compare deterministic output native-vs-interpreter-vs-JIT | M2 (basic), stronger after M4 |

## Conventions

- **Task IDs:** `M<n>-T<k>` for milestones (e.g. `M1-T3`), `INT-T<k>` for the integration track. Use them in branch names, commits, and GitHub issue titles.
- **Checkboxes** track progress. Check a box only when the task's acceptance line is met.
- **Test-first for bugs:** every bug found (in the field or by the fuzzer) becomes a regression vector *before* the fix (testing.md §6.3) — that's not a milestone task, it's a standing rule.
- **Don't pre-empt.** A task tagged for a later milestone stays untouched until its milestone (spec.md §16, [conventions](../agents/conventions.md)).
- References like `(§7.1)` point at spec.md; `(T§6.1)` points at testing.md.
- Keep [`../agents/status.md`](../agents/status.md) in sync as milestones advance.
