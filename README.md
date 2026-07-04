# x86jit

[![CI](https://github.com/mikolajmikolajczyk/x86jit/actions/workflows/ci.yml/badge.svg)](https://github.com/mikolajmikolajczyk/x86jit/actions/workflows/ci.yml)

An x86-64 → host recompiler (JIT), delivered as a pure-Rust library.

`x86jit` executes x86-64 guest code on any host (x86-64 or ARM64) via JIT recompilation. The core is **guest-agnostic** — it knows nothing about PS4, ELF, the syscalls of any concrete OS, or GPUs. It's a "CPU engine": you give it memory plus an entry point, it runs instructions and yields control every time it hits something it doesn't handle itself.

- **In scope:** x86-64 decoding (via `iced-x86`), lift to a custom IR, two backends (interpreter + Cranelift JIT), translation cache, dispatcher loop, guest memory + CPU state, return-based `Exit` API.
- **Out of scope (the embedder's job):** file-format parsing (ELF/SELF/PE), OS syscall emulation (HLE), MMIO/devices/GPU, loaders, high-level thread scheduling.

The full design lives in [`spec.md`](wiki/design/spec.md).

## Workspace

```
x86jit-core/        # Vm, Vcpu, IR, lift, cache, dispatcher, interpreter — the engine
x86jit-cranelift/   # Cranelift JIT backend (feature `jit`, optional)
x86jit-elf/         # optional ELF-segment loader helper (convenience, not core)
x86jit-tests/       # differential testing, instruction corpus, fuzzing
```

## Status

Early scaffold (milestone M0). Public API types are defined and the dispatcher loop is wired; the engine internals are `todo!()` stubs filled in milestone order (see [`wiki/agents/status.md`](wiki/agents/status.md) and `spec.md` §12).

## Getting started

With Nix (recommended — pins the whole toolchain):

```sh
nix develop            # or: direnv allow, then it auto-loads
cargo build
cargo nextest run
```

Without Nix:

```sh
rustup toolchain install stable   # MSRV 1.75
cargo build
cargo test
```

## Documentation

- [`spec.md`](wiki/design/spec.md) — authoritative design spec (contract, IR, backends, milestones, traps).
- [`wiki/`](wiki/) — load-on-demand knowledge tree (agent + user docs, ADRs, decision log).
- [`AGENTS.md`](AGENTS.md) / [`CLAUDE.md`](CLAUDE.md) — pointer table for coding agents.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option. All core dependencies are permissive (MIT/Apache), so there are no copyleft constraints (`spec.md` §15).
