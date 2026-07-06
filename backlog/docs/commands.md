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
pre-commit install                                  # one-time, per clone
pre-commit run --all-files                          # run active hooks
pre-commit run --all-files --hook-stage manual      # include staged-as-manual hooks
```

## Git / GitHub

```sh
gh issue list
gh issue view <n>
gh pr create --fill
gh pr list
```
