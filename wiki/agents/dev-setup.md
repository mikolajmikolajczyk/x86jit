# Dev setup

Toolchain and local-environment setup.

## Nix flake (primary)

`flake.nix` pins the toolchain via rust-overlay (stable rustc/cargo + `rust-src`, `rust-analyzer`, `clippy`, `rustfmt`, targets `x86_64` and `aarch64` Linux — the two host classes in §1). Enter the devShell:

```sh
nix develop
```

It provides rust, cargo, rust-analyzer, clippy-driver, rustfmt, and `cargo-nextest`. `nix build` builds the package output; `nix flake check` evaluates outputs.

No system libraries are required: `iced-x86`, `cranelift`, and `memmap2` are pure Rust (memmap2 uses libc `mmap`/`mprotect` only).

## direnv (optional but recommended)

If `.envrc` ships in the repo with `use flake`, allow it once per clone so the devShell auto-loads on `cd`:

```sh
direnv allow
```

## Fallback: host rustup

The flake is primary but not mandatory. A host toolchain works too:

```sh
rustup toolchain install stable
cargo build
```

Keep the host toolchain ≥ MSRV (1.75).

## Pre-commit

If `.pre-commit-config.yaml` is present:

```sh
pre-commit install
pre-commit run --all-files
pre-commit run --all-files --hook-stage manual   # includes manual-staged hooks
```

Typical hooks: whitespace, EOF, YAML/JSON checks, markdownlint, `cargo fmt --check`, `cargo clippy`, gitleaks, GPG UID guard.

## GPG signing

The `gpg-uid-guard` hook (if configured) refuses to sign when `user.email` has no matching UID on `user.signingkey`. Fix path:

```sh
git config user.email <your-email>
git config user.signingkey <key-id>
# or attach a matching UID to the key with `gpg --edit-key <key>`
```
