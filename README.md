# x86jit

[![CI](https://github.com/mikolajmikolajczyk/x86jit/actions/workflows/ci.yml/badge.svg)](https://github.com/mikolajmikolajczyk/x86jit/actions/workflows/ci.yml)

An x86-64 → host recompiler (JIT), delivered as a pure-Rust library.

> ⚠️ **Early-stage — not production quality.** Started July 2026, under active development.
> It almost certainly has bugs and missing instructions. Be clear-eyed about what the
> testing buys you: a differential oracle validates the instructions that **are** lifted
> (interpreter vs JIT vs a real CPU), but it can't tell you what's *missing* — gaps surface
> when real code hits an unimplemented instruction and traps. See [Status](#status).
>
> **Need a production-grade x86 emulator today? Use [QEMU](https://www.qemu.org/) or
> [Unicorn](https://www.unicorn-engine.org/).** x86jit is for people who want an
> embeddable, hackable CPU core in pure Rust and can live with gaps.

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

Actively developed, with a strong oracle for the instructions it *does* implement. A
hand-written instruction corpus and a fuzzer cross-check the **lifted** instructions three
ways — interpreter vs JIT, and both against a real CPU (Unicorn + native execution) — on
both an **x86-64 and an AArch64** CI runner, so the ARM host path is validated, not
assumed. Important caveat: the corpus validates *what's lifted*; it does **not** tell you
what's missing — that only surfaces when real code hits an unimplemented instruction.

**Unmodified real programs that run our test workloads** (interpreter and JIT produce the same output as running them natively — these are specific scripts/inputs, not each project's full test suite):

- busybox applets — `sha256sum`, `wc`, `sort`, `awk`, gzip
- sqlite3, lua, libjpeg-turbo `djpeg`, and **CPython 3.13**
- static, static-PIE, and dynamically-linked executables against **both musl and glibc**
- multi-process shell pipelines run straight out of a **Docker/OCI image**

**Instruction coverage:** the full scalar integer set plus SSE/SSE2 up through the
common AVX/AVX2 vector set — SSE3/SSSE3/SSE4.1/SSE4.2, AVX, AVX2, BMI1/BMI2,
`tzcnt`/`lzcnt`/`movbe`, and **true 80-bit x87** computed in software (so x87 results
are bit-identical on x86-64 and ARM64). AVX-512/EVEX is partial and growing. The
guest CPU feature set is selectable per run (`baseline` / `v2` / `v3` / `v4`, the way
`qemu -cpu` works) rather than hardcoded. The exact per-generation breakdown of which
encodings lift is a generated, CI-checked artifact — see the
[**instruction-coverage map**](backlog/docs/compat/isa-coverage.md).

**Engine:** two interchangeable backends — a portable interpreter and a Cranelift
JIT — over a single IR, with a translation cache, hotness-gated tier-up, superblock
regions, and block chaining + indirect-branch caching for fast dispatch. Self-modifying
code stays coherent, multiple guest threads share one VM, and x86-TSO memory-ordering
is preserved on weak (ARM) hosts — all exercised on the AArch64 runner.

**Performance.** Not yet optimized — expect roughly an **order of magnitude slower than
native** for hot code (a tight scalar loop is ~20× native on the JIT; the interpreter is
~40–250×), and worse for startup-heavy or run-once code, where the JIT pays to compile
everything up front. Throughput work is ongoing. The `x86jit-bench` crate records
interp/JIT/native timings per commit if you want real numbers.

**Known gaps** (deliberately absent or partial today):

- AVX-512 / EVEX is partial and growing; MMX is minimal (guests generally use SSE instead).
- 64-bit long mode + 32-bit protected mode only — **no 16-bit real mode** (BIOS / boot code).
- Segmentation is limited to the `FS`/`GS` base (modern TLS); no full segment-descriptor model.
- Signals and fork/exec *after* a process spawns threads are not fully modeled (single-threaded fork/exec works; the threaded case returns a defined error rather than guessing).
- OS emulation (syscalls, devices, loaders) is the embedder's job, not the core's. The bundled Linux shim covers what the test programs need and is extended on demand.

**API stability.** Pre-1.0 (`0.x`). The embedding API (`Vm`, `Vcpu`, `Exit`, …) is not
frozen and will have breaking changes between releases.

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

- [`spec.md`](backlog/docs/design/spec.md) — authoritative design spec (contract, IR, backends, semantics traps).
- [`backlog/`](backlog/) — load-on-demand knowledge tree (agent + user docs, ADRs, decision log).
- [`AGENTS.md`](AGENTS.md) / [`CLAUDE.md`](CLAUDE.md) — pointer table for coding agents.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option. All core dependencies are permissive (MIT/Apache), so there are no copyleft constraints (`spec.md` §15).
