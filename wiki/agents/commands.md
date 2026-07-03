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

## Typecheck / lint / format

```sh
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
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
