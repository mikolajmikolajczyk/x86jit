---
id: doc-2
title: 'Commands'
type: guide
created_date: '2026-07-06 11:25'
---

# Commands

Everyday commands for this project. Keep this file **flat and copy-pasteable** — agents and humans both grep it.

All commands assume you're inside the devShell (`nix develop`, or auto-loaded via direnv). See [`dev-setup.md`](dev-setup.md).

## Build / run / test

```sh
cargo build                      # whole workspace
cargo build -p x86jit-core       # single crate
cargo test                       # all tests (unit + integration)
cargo nextest run                # faster test runner (shipped in the devShell)
cargo run -p <bin>               # once a binary target exists
```

The JIT backend is behind a default feature `jit` on `x86jit-cranelift`:

```sh
cargo build -p x86jit-cranelift                       # with JIT (default)
cargo build -p x86jit-cranelift --no-default-features # core-only, no cranelift pulled
```

The Unicorn differential oracle is behind the `unicorn` feature on `x86jit-tests`
(off by default; links nixpkgs `libunicorn` via pkg-config — needs the devShell):

```sh
cargo nextest run                                     # default: no native deps
nix develop -c cargo nextest run -p x86jit-tests --features unicorn   # interp-vs-Unicorn diff
# capture a snippet into a permanent .ron vector (Unicorn is the oracle):
nix develop -c cargo run -p x86jit-tests --features unicorn --bin capture -- \
  --bytes <hex> --init "rax=0x..,rbx=.." --name <id> --tags <a,b> --out x86jit-tests/vectors/<cat>/
```

⚠️ Don't pass `--all-features` outside the devShell — it enables `unicorn`, whose
sys crate needs `libclang` + `pkg-config unicorn` (provided by `nix develop`).

## The real-program ladder (unemulinux)

The corpus here validates *what is lifted*. What real software trips over is the
ladder — busybox, sqlite, CPython, the Go servers, caddy — and since the split it
runs in [`unemulinux`](https://github.com/unemu-org/unemulinux). `scripts/ladder.sh`
drives it from here:

```sh
scripts/ladder.sh                     # smoke subset vs the working tree (~30 s)
scripts/ladder.sh --full              # the whole ladder (~10 min)
scripts/ladder.sh --rev HEAD~5        # a specific revision, in a detached worktree
scripts/ladder.sh --full -- -E 'test(caddy)'   # extra nextest args after --
```

It needs a sibling `../unemulinux` checkout (or `UNEMULINUX_DIR`). Without one it
exits **2**, not 0 — "the ladder did not run" must never look like "the ladder
passed". The `--if-present` form, used by the pre-push hook, downgrades that to a
printed skip.

The smoke subset is one static musl program, one dynamic glibc program, one Go binary
and one busybox applet — four different failure classes. It is **not** the ladder:
sqlite, lua, CPython, djpeg, caddy, the OCI rungs and the threading paths are all
outside it, and the script says so every time it finishes.

`--rev` pins the *recompiler*; the userland is always the working tree next door, so
a harness change and the engine change it exercises can be tested together.

## Release-profile runs

`cargo nextest run` builds the **dev** profile, so every test exercises the engine at
`opt-level 0`. A defect that only appears under optimization is invisible to all of
them — task-223 was exactly that, and its regression test passed under `cargo test`
while failing under `cargo test --release` on the same tree.

```sh
cargo nextest run --release --features unicorn -E 'not binary(fuzz_robustness)'
cargo nextest run --release -p x86jit-tests -E 'binary(jit) or binary(differential)'
```

CI runs the second (narrower) form; the full release run is a local check worth doing
before a release or after touching the softfloat and codegen paths.

## AVX/VEX differential fuzz (`cargo xfuzz`)

```sh
cargo xfuzz --secs 60                  # register operands (the historical campaign)
cargo xfuzz --mem --secs 60            # the same programs with MEMORY source operands
cargo xfuzz --mem --seed 206           # replay exactly one program
cargo xfuzz --list                     # every op, grouped by family
```

`--mem` (task-325) rewrites each memory-capable VEX op to take its last source from
memory, over a two-page region so an operand can be 32-byte aligned, arbitrarily
unaligned, or straddling a page boundary. It is the only leg that can falsify
memory-source decoding, effective-address computation and load width — the register
campaign counts an op covered on the strength of its register form alone.

Expect `UnknownInstruction` findings from it: those are memory forms the lifter does not
have, the same set the coverage map reports as `reg_only`. A **wrong result** with no
trap is the interesting kind.

## Typecheck / lint / format

```sh
cargo clippy --all-targets -- -D warnings                 # default features (no native deps)
nix develop -c cargo clippy -p x86jit-tests --features unicorn --all-targets -- -D warnings
cargo fmt --all                  # write
cargo fmt --all -- --check       # verify (CI)
```

## Nix

```sh
nix develop                      # enter devShell (toolchain + nextest)
nix flake check                  # evaluate flake outputs
nix build                        # build the package output
nix flake metadata               # inspect resolved inputs / lock
```

## Pre-commit

```sh
pre-commit install --install-hooks                  # one-time, per clone (pre-commit + pre-push)
pre-commit run --all-files                          # run active hooks
pre-commit run --all-files --hook-stage manual      # include staged-as-manual hooks
```

Stages: **pre-commit** = hygiene + `cargo fmt --check`; **pre-push** = `cargo clippy -D warnings`
+ the perf gate.

## Performance (bench + regression gate)

Always `--release` (debug timings are meaningless). See [`design/`] and `bench/README.md`.

```sh
cargo run -p x86jit-bench --release -- record        # measure HEAD; write bench/history/<sha>.json,
                                                     # bench/baseline.json, backlog/docs/performance.md
cargo run -p x86jit-bench --release -- gate          # compare HEAD vs baseline; exit 1 on >10% regression
cargo run -p x86jit-bench --release -- compare A B    # delta table between two records
```

The **pre-push perf gate** (`scripts/perf-gate.sh`) blocks a push whose interpreter or JIT time
regresses more than `X86JIT_PERF_THRESHOLD` percent (default 10) vs `bench/baseline.json`:

```sh
X86JIT_ALLOW_PERF_REGRESSION=1 git push              # override an intended/accepted regression
cargo run -p x86jit-bench --release -- record        # then accept it as the new baseline + commit
git add bench/baseline.json backlog/docs/performance.md bench/history/
```

The gate skips when there's no baseline (fresh clone) or the host differs (timings aren't comparable
across machines). `performance.md` (Backlog.md doc-21) shows each snapshot's Δ vs the prior baseline.

## Git / GitHub

```sh
gh issue list
gh issue view <n>
gh pr create --fill
gh pr list
```
