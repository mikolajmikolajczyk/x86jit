# x86jit

[![CI](https://github.com/unemu-org/x86jit/actions/workflows/ci.yml/badge.svg)](https://github.com/unemu-org/x86jit/actions/workflows/ci.yml)

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
x86jit-tests/       # differential testing (vs Unicorn + native), instruction corpus, fuzzing, harness
x86jit-bench/       # workload timings (interp vs JIT vs native), recorded per commit
```

This repository is the **recompiler only** — it emulates no operating system. A
Linux syscall shim, a process model, an OCI/ELF runner and the whole-program test
ladder (busybox, sqlite, CPython, Go servers) live in
[`unemulinux`](https://github.com/unemu-org/unemulinux), which embeds this library.
That split is not aspirational: `x86jit-core`'s dependency set is exactly
`{iced-x86}`, and a test (`x86jit-tests/tests/boundary.rs`) fails the build if that
ever changes.

## Status

Actively developed, with a strong oracle for the instructions it *does* implement. A
hand-written instruction corpus and a fuzzer cross-check the **lifted** instructions three
ways — interpreter vs JIT, and both against a real CPU (Unicorn + native execution) — on
both an **x86-64 and an AArch64** CI runner, so the ARM host path is validated, not
assumed.

One limit on that, stated because it is easy to read past. The corpus validates *what's
lifted*; it does **not** tell you what's missing — that surfaces only when real code hits
an unimplemented instruction.

The native comparison covers general registers, flags, the whole vector file
(XMM/YMM/ZMM**0–31**), the opmasks, the x87 register stack and control word, and MXCSR.
Two things are captured but deliberately **not compared**, because the engine does not
model them: the x87 status-word condition codes and tag word (`TASK-324`), and MXCSR's
six sticky exception flags (`deferred.md`). Both show up in full in a divergence report.

**Unmodified real programs run on this engine** — busybox applets (`sha256sum`, `wc`,
`sort`, `awk`, gzip), sqlite3, lua, libjpeg-turbo `djpeg`, **CPython 3.13**, Go servers,
static/static-PIE/dynamic executables against both musl and glibc, and multi-process shell
pipelines out of a Docker/OCI image. Interpreter and JIT produce the same output as running
them natively.

Those tests **live in [`unemulinux`](https://github.com/unemu-org/unemulinux)**, not here,
because running a real program needs an operating system and this repository deliberately
has none. They still run — a lifter change is validated against that ladder before it is
considered good; the check simply spans two repositories now, so it is a CI-plumbing
problem rather than a coverage one. What runs *in this repository's own* CI is the
ISA-level validation below.

**Instruction coverage:** the full scalar integer set plus SSE/SSE2 up through the
common AVX/AVX2 vector set — SSE3/SSSE3/SSE4.1/SSE4.2, AVX, AVX2, BMI1/BMI2,
`tzcnt`/`lzcnt`/`movbe`, and **true 80-bit x87** computed in software (so x87 results
are bit-identical on x86-64 and ARM64). AVX-512/EVEX is partial and growing. The
guest CPU feature set is selectable per run (`baseline` / `v2` / `v3` / `v4`, the way
`qemu -cpu` works) rather than hardcoded. The exact per-generation breakdown of which
encodings lift is a generated, CI-checked artifact — see the
[**instruction-coverage map**](backlog/docs/compat/isa-coverage.md). **Read it as an
upper bound:** an instruction with both a register and a memory form is probed as the
register form, so a missing memory-operand form can still be reported as lifted
(`TASK-312`). That is how `vextract*`'s memory destination stayed invisible until a real
binary trapped on it.

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
- OS emulation (syscalls, devices, loaders) is the embedder's job, not the core's — see [`unemulinux`](https://github.com/unemu-org/unemulinux) for a Linux userland built on this library.
- **`Prot` is advisory — nothing enforces it.** A guest store into a region mapped `R` or `RX` succeeds and changes the bytes, on both backends; the engine models no permission fault (`TASK-330`). Map read-only expecting a trap and you get silent corruption instead.
- **MXCSR governs nothing** on the SSE side — `stmxcsr` returns the reset value, `ldmxcsr` is a no-op, and no SSE floating-point exception is raised or reported (`deferred.md`). The x87 half is modelled: all six status-word flags, the stack-fault flag with C1, the condition codes, and `#MF` delivery, each validated against a real CPU.
- **Self-modifying code is observed one block late.** A block that writes into the page it is itself executing runs to the end of that block on the old bytes; the re-lift takes effect on the next dispatch. This matches QEMU and is the deviation `spec.md` §10 records. Everything else — another block's page, `rep stos`, an x87 store — invalidates on both backends.

**API stability.** Pre-1.0 (`0.x`). The embedding API (`Vm`, `Vcpu`, `Exit`, …) is not
frozen and will have breaking changes between releases.

## Getting started

With Nix (recommended — pins the whole toolchain):

```sh
nix develop            # or: direnv allow, then it auto-loads
cargo build
cargo nextest run
```

Neither the build nor the tests need the network. The `oracles/` submodule holds the
pinned manuals the code cites (see [`PROVENANCE.md`](PROVENANCE.md)); it is only needed
if you want to *read* a source, so a clone without it builds and tests fine:

```sh
git submodule update --init      # optional: the cited sources, for humans
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
- [`PROVENANCE.md`](PROVENANCE.md) — where the engine's behaviour comes from: what is authoritative (the Intel SDM, the AMD APM, the psABI), what is merely an oracle, what we deliberately do not model, and the licence surface.
- [`backlog/`](backlog/) — load-on-demand knowledge tree (agent + user docs, ADRs, decision log).
- [`AGENTS.md`](AGENTS.md) / [`CLAUDE.md`](CLAUDE.md) — pointer table for coding agents.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option. All core dependencies are permissive (MIT/Apache), so there are no copyleft constraints (`spec.md` §15).
