# x86jit

[![CI](https://github.com/mikolajmikolajczyk/x86jit/actions/workflows/ci.yml/badge.svg)](https://github.com/mikolajmikolajczyk/x86jit/actions/workflows/ci.yml)

An x86-64 → host recompiler (JIT), delivered as a pure-Rust library.

`x86jit` executes x86-64 guest code on any host (x86-64 or ARM64) via JIT recompilation. The core is **guest-agnostic** — it knows nothing about PS4, ELF, the syscalls of any concrete OS, or GPUs. It's a "CPU engine": you give it memory plus an entry point, it runs instructions and yields control every time it hits something it doesn't handle itself.

- **In scope:** x86-64 decoding (via `iced-x86`), lift to a custom IR, two backends (interpreter + Cranelift JIT), translation cache, dispatcher loop, guest memory + CPU state, return-based `Exit` API.
- **Out of scope (the embedder's job):** file-format parsing (ELF/SELF/PE), OS syscall emulation (HLE), MMIO/devices/GPU, loaders, high-level thread scheduling.

The full design lives in [`spec.md`](backlog/docs/design/spec.md).

## Workspace

The **core** is guest-agnostic; everything else is an embedder or tooling crate.

```
x86jit-core/        # Vm, Vcpu, IR, lift, cache, dispatcher, interpreter, x87/f80 — the engine
x86jit-cranelift/   # Cranelift JIT backend (the second `Backend`)
x86jit-elf/         # ELF loader helpers (static / static-PIE / dynamic + stack setup)
x86jit-linux/       # a Linux syscall shim + process scheduler (fork/exec/wait/pipe) — an embedder
x86jit-cli/         # runs a program: a host x86-64 binary (`run`) or a `docker save` image (`oci`)
                    #   — lib + binary; folds in the OCI image reader (was x86jit-oci) and the runner glue (was x86jit-run)
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
- SSE/SSE2 through the x86-64-v3 scalar+vector set: SSE4.2, AVX/AVX2, BMI1/BMI2, `tzcnt`/`lzcnt`/`movbe`; **true 80-bit x87** (software extended float, so identical on x86-64 and ARM64). AVX-512/EVEX is in progress.
- **The guest CPU is embedder-configurable per run** — `GuestCpuFeatures` presets `baseline`/`v2`/`v3`/`v4` drive CPUID/XCR0 like `qemu -cpu`, instead of a hardcoded set. The Cranelift backend's host codegen ISA is a separate `HostTarget` knob.
- Self-modifying-code coherence, multithreading over `Arc<Vm>`, and x86-TSO memory-ordering barriers exercised on a **real AArch64 CI runner**.
- A Linux embedder that runs multi-process shell pipelines out of a Docker image.

See [`backlog/agents/status.md`](backlog/agents/status.md) for the detailed feature map and `spec.md` §12 for the milestones.

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

## Embedding

The core is a library. Give it a memory map and an entry point; it hands control
back through `Exit` whenever it hits something you own (a syscall, an MMIO
access, an unsupported instruction):

```rust
use x86jit_core::{Exit, Prot, Reg, RegionKind, Vm, VmConfig};

let mut vm = Vm::new(VmConfig::flat(0x1_0000));   // flat guest space, interpreter backend
vm.map(0, 0x1_0000, Prot::RWX, RegionKind::Ram).unwrap();
vm.write_bytes(0x1000, &[0xB8, 0x05, 0x00, 0x00, 0x00, 0xF4]).unwrap(); // mov eax,5 ; hlt

let mut cpu = vm.new_vcpu();
cpu.set_reg(Reg::Rip, 0x1000);
assert!(matches!(cpu.run(&vm, None), Exit::Hlt));
assert_eq!(cpu.reg(Reg::Rax) as u32, 5);
```

Swap in the JIT with `Vm::with_backend(cfg, Box::new(JitBackend::new()))` — same
API, identical guest state. Runnable examples:

```sh
cargo run -p x86jit-core      --example raw_bytes      # smallest embedding
cargo run -p x86jit-core      --example mmio_device    # a trapped MMIO device
cargo run -p x86jit-cranelift --example jit_vs_interp  # wiring in the JIT
cargo run -p x86jit-elf       --example run_elf -- ELF # load + run a static ELF
```

## Documentation

- [`spec.md`](backlog/docs/design/spec.md) — authoritative design spec (contract, IR, backends, milestones, traps).
- [`backlog/`](backlog/) — load-on-demand knowledge tree (agent + user docs, ADRs, decision log).
- [`AGENTS.md`](AGENTS.md) / [`CLAUDE.md`](CLAUDE.md) — pointer table for coding agents.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option. All core dependencies are permissive (MIT/Apache), so there are no copyleft constraints (`spec.md` §15).
