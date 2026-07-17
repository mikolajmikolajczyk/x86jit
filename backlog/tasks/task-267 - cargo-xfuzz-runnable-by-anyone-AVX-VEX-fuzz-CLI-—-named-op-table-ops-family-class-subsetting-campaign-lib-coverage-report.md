---
id: TASK-267
title: >-
  cargo xfuzz: runnable-by-anyone AVX/VEX fuzz CLI — named op table,
  --ops/--family/--class subsetting, campaign lib, coverage report
status: In Progress
assignee: []
created_date: '2026-07-17 19:12'
updated_date: '2026-07-17 19:28'
labels:
  - tooling
  - test
  - fuzz
  - simd
dependencies:
  - TASK-264
ordinal: 297000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Turn the AVX fuzz driver into a first-class tool anyone can point at a specific instruction, without env-var incantations or editing source. Today the only entry point is an #[ignore]d test driven by FUZZ_SECONDS/FUZZ_OPS env vars — undiscoverable and unpleasant. Root reasons: (1) x86jit_cranelift is a [dev-dependency] of x86jit-tests, so a real src/bin can't link the JIT backend, forcing the #[ignore]d-test shape; (2) the VEX op pool is an opaque match op { 0 => a.vpaddsb(...), .. } with a hand-synced magic const V_VEX_OPS=63 that has already drifted (fallback arm uses op%7 while the sig dedup uses op%63) — there is no way to name or select an op.

Deliverables:

1. Move x86jit_cranelift from [dev-dependencies] to [dependencies] in x86jit-tests/Cargo.toml (the crate is publish=false and already has a [[bin]] capture precedent), then add a real [[bin]] fuzz at src/bin/fuzz.rs.

2. Replace the positional VEX op table with a named data table: a &[VexOp] where VexOp { name: &'static str, family: Family, emit: fn(&mut CodeAssembler, d,a,b,imm) }. This one change is the keystone — it powers --ops, --family, --list, per-op coverage, and named findings, and it deletes the magic constant + the op%7/op%63 drift.

3. Extract the two-leg (jit-vs-interp + native-vs-interp) + shrink + dedup + log loop currently inlined in tests/fuzz_avx.rs into pub fn run_campaign(cfg) -> Report in the lib, so drivers are ~10 lines and a fast #[test] can still exercise the machinery under nextest.

4. CLI (clap or hand-rolled) with a cargo alias .cargo/config.toml [alias] xfuzz = 'run --release -p x86jit-tests --bin fuzz --':
   - cargo xfuzz --list                       (print families + op names + counts)
   - cargo xfuzz --ops vcvtps2ph              (subset the pool BEFORE generation)
   - cargo xfuzz --family convert,fma
   - cargo xfuzz --class vex --secs 3600 --len 12
   - cargo xfuzz --seed 1964 --ops vcvtps2ph  (replay one finding deterministically)
   Bare 'cargo xfuzz' = a bounded 60s smoke over everything that prints the coverage table — never a silent multi-hour run.

5. Per-op coverage counters (generated / native_run / diverged), printed at end. This is the honesty fix: the first status line must show the native-leg coverage fraction, so a '0 bugs' result is auditable instead of hiding a 17%-coverage run.

6. Every finding logs its own copy-paste repro line, e.g. 'repro: cargo xfuzz --seed 1964 --ops vcvtps2ph', so a finding is reproducible by someone who did not build the harness.

Naming: do NOT call it 'cargo fuzz' — that shadows cargo-fuzz (libfuzzer). 'xfuzz' avoids the collision.

Depends on TASK-264 (the driver + VVex pool this refactors). The generator-selection rewrite here also subsumes replacing the avx: bool with a mode/class-aware arm table; keep the gen()/gen32() RNG streams byte-identical so existing seeds in tests/fuzz.rs keep their meaning.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 cargo xfuzz --list prints every VEX op by name grouped by family, with counts, and the magic const V_VEX_OPS plus the op%7/op%63 fallback drift are both gone
- [ ] #2 cargo xfuzz --ops <name>[,<name>] and --family <f>[,<f>] and --class <c> subset the generated pool before generation; --seed replays one program deterministically
- [ ] #3 x86jit_cranelift moved to [dependencies]; a real [[bin]] fuzz exists; the campaign loop lives in pub fn run_campaign in the lib and a fast (<=5s) #[test] exercises it under nextest
- [ ] #4 Run end prints a per-op coverage table (generated / native_run / diverged) and the native-leg coverage fraction appears in the FIRST periodic status line, not only in the final summary
- [ ] #5 Each logged finding includes a copy-paste 'repro: cargo xfuzz --seed N --ops NAME' line that reproduces it
- [ ] #6 gen()/gen32() produce byte-identical RNG streams to before, so pre-existing seeds in tests/fuzz.rs keep their behaviour; cargo nextest -E 'not binary(fuzz_robustness)' and clippy -D warnings pass
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
