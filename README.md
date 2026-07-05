# x86jit

[![CI](https://github.com/mikolajmikolajczyk/x86jit/actions/workflows/ci.yml/badge.svg)](https://github.com/mikolajmikolajczyk/x86jit/actions/workflows/ci.yml)

An x86-64 → host recompiler (JIT), delivered as a pure-Rust library.

`x86jit` executes x86-64 guest code on any host (x86-64 or ARM64) via JIT recompilation. The core is **guest-agnostic** — it knows nothing about PS4, ELF, the syscalls of any concrete OS, or GPUs. It's a "CPU engine": you give it memory plus an entry point, it runs instructions and yields control every time it hits something it doesn't handle itself.

- **In scope:** x86-64 decoding (via `iced-x86`), lift to a custom IR, two backends (interpreter + Cranelift JIT), translation cache, dispatcher loop, guest memory + CPU state, return-based `Exit` API.
- **Out of scope (the embedder's job):** file-format parsing (ELF/SELF/PE), OS syscall emulation (HLE), MMIO/devices/GPU, loaders, high-level thread scheduling.

The full design lives in [`spec.md`](wiki/design/spec.md).

## Workspace

The **core** is guest-agnostic; everything else is an embedder or tooling crate.

```
x86jit-core/        # Vm, Vcpu, IR, lift, cache, dispatcher, interpreter, x87/f80 — the engine
x86jit-cranelift/   # Cranelift JIT backend (the second `Backend`)
x86jit-elf/         # ELF loader helpers (static / static-PIE / dynamic + stack setup)
x86jit-linux/       # a Linux syscall shim + process scheduler (fork/exec/wait/pipe) — an embedder
x86jit-oci/         # `docker save` image parser (rootfs + config) — an embedder
x86jit-run/         # runs an OCI/Docker image on the engine (glue over the above)
x86jit-tests/       # differential testing (vs Unicorn + native), instruction corpus, fuzzing, harness
x86jit-bench/       # workload timings (interp vs JIT vs native), recorded per commit
```

## Status

Mature. All milestones (M0–M8 + integration) are complete; the interpreter and the
JIT agree with each other, with Unicorn, and with native execution across the
corpus, a fuzzer, and a ladder of **unmodified real programs** — busybox
(`sha256sum`/`wc`/`sort`/`awk`/gzip), sqlite3, lua, libjpeg-turbo `djpeg`, and
**CPython 3.13** — plus dynamically-linked musl **and** glibc binaries and real
**OCI/Docker images** run three ways. Highlights:

- Two backends over one IR, hotness-gated tier-up, superblocks, block chaining + IBTC dispatch.
- Full SSE/SSE2 + the x86-64-v2 (Jaguar) SSE4.2 set; **true 80-bit x87** (software extended float, so identical on x86-64 and ARM64).
- Self-modifying-code coherence, multithreading over `Arc<Vm>`, and x86-TSO memory-ordering barriers exercised on a **real AArch64 CI runner**.
- A Linux embedder that runs multi-process shell pipelines out of a Docker image.

See [`wiki/agents/status.md`](wiki/agents/status.md) for the detailed feature map and `spec.md` §12 for the milestones.

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
