---
id: doc-17
title: 'Running OCI/Docker images on x86jit — planning brief (for Fable 5)'
type: specification
created_date: '2026-07-06 11:25'
---

# Running OCI/Docker images on x86jit — planning brief (for Fable 5)

Input for a phased plan. Goal: **read and run OCI/Docker container images on the
x86jit recompiler**, offline, starting from the simplest and climbing the real
Linux userland surface. Deliverable: a rigorous, professionally-structured,
independently-landable phased plan that will not collapse under its own weight.

## The idea (and why it's tractable)

A Docker/OCI **image** is *not* a running container — it's a tarball: filesystem
layers (tars, stacked with whiteouts) + a `config.json` (`Env`, `Cmd`,
`Entrypoint`, `WorkingDir`, `User`). Reading it needs **no kernel**. Container
*runtime* machinery (namespaces, cgroups, overlayfs, network) is about
*isolation/limits*, which x86jit does not need to *execute* the payload — we run
the guest in our own sandbox. So: extract layers → rootfs dir; read config → what
to run; load the entrypoint ELF into the JIT, serve the rootfs through the syscall
shim, set env/argv/cwd; run three ways (native == interpreter == JIT).

The image is therefore the **standard input format** for the "climb real binaries"
track (AGENTS.md §A, the highest-value forcing function) — an infinite corpus of
real software instead of hand-picked binaries.

Start offline: user provides `docker save foo > foo.tar`. Registry pull (network)
is later.

## Hard architectural rule — keep the CORE clean

`x86jit-core` is a **guest-agnostic** x86-64→host recompiler (spec §1): "File-format
parsing, OS syscall emulation, and devices live in the embedder's code, not the
core." This MUST hold. The plan must specify, per component, where code lands:

- **May extend the CORE**: *guest instruction semantics only* — new x86-64
  instructions (lift + interp + JIT backend + differential test). That is
  legitimately the recompiler's job. Nothing else.
- **Must live in EMBEDDER crate(s), never core**: OCI image parsing (tar/JSON),
  rootfs assembly, the syscall shim / OS emulation, the process model
  (fork/execve/wait/pipe), file-format loading, devices, networking.

Today the syscall shim lives in `x86jit-tests/src/syscall.rs` (~1000 lines, ~100
syscalls) — that is *test-harness* code. Running images is a real capability, not a
test fixture. The plan must decide the crate structure: e.g. a new `x86jit-linux`
(or `x86jit-os`) embedder crate owning the shim + process model + ELF/loader glue,
an `x86jit-oci` crate owning image parsing/rootfs, and a thin `x86jit-run` binary
tying them together. Propose the exact crate boundaries and dependency directions
(core depends on nobody; embedder crates depend on core; the runner depends on
embedder crates) so the boundary cannot erode.

## Instruction policy — add real instructions, never dumb down images

If an image needs newer instructions, **add the instructions** (real
lift/interp/JIT/oracle-tested support). Do NOT rebuild images without SSE4/AVX, do
NOT pick artificially old images to dodge gaps, do NOT stub instructions to
"mostly work." Every added instruction is validated interp == JIT == Unicorn
(testing.md). A missing instruction is a logged forcing-function gap that gets
*filled*, not avoided.

## Compatibility map per instruction-set generation (a first-class deliverable)

Build a **machine-checked coverage map** of the x86-64 instruction-set generations,
so we see *realistically* what we have vs. lack — and so it cannot rot:

- Generations to model: **baseline x86-64-v1** (SSE, SSE2), **v2** (SSE3, SSSE3,
  SSE4.1, SSE4.2, POPCNT, CMPXCHG16B), **v3** (AVX, AVX2, BMI1/2, FMA, MOVBE,
  LZCNT), **v4** (AVX-512 family). Plus scalar groups (x87 state, cmpxchg, string
  ops) and the CPUID feature bits we advertise (source: `cpuid_helper`).
- The map should be **derived from what's actually implemented** (the lift/decoder
  coverage + the CPUID bits we report), not hand-maintained prose that drifts.
  Propose the mechanism: e.g. a coverage manifest checked in CI against the set of
  iced-x86 mnemonics the lifter handles, emitting a per-generation
  implemented/missing table. Decide how CPUID advertisement stays consistent with
  real coverage (advertising a feature we don't fully lift is a trap — glibc/apps
  probe CPUID and then *use* the feature).
- Today (ground truth to verify): baseline SSE/SSE2 + parts of SSSE3/SSE4 (pshufb,
  popcnt, crc32, pcmpgtq/eqq, pextrb…), **no AVX**, f64-backed x87 (no true 80-bit).
  The map must make this precise and per-feature.

This map is the **gating tool**: it tells us which images (by their build
baseline — most modern distro images target v2 or v3) can run, and which
instructions to add next to unlock the next tier.

## Where the real work is (sequence the climb)

The *format* is easy; the *contents* exercise far more Linux surface than we cover.
The plan must sequence rungs by rising engine cost:

1. **Image loader + runner MVP** — parse `docker save` tar, apply layers → rootfs,
   read config, run a **single static x86-64 ELF entrypoint, no fork**. First
   target: `hello-world` (its image is one static binary). Three-way.
2. **Static single-process climb** — distroless/scratch static (Go/Rust static),
   static busybox commands. Surfaces syscalls (`getrandom`, `pipe`, more `fstat`
   shapes) and instructions. Fill gaps.
3. **Dynamic glibc at scale** — real distro base images (`debian`, `alpine`+musl):
   ld-linux + many `.so`s. We have glibc dynamic linking (DYN-T5) but images pull
   far more. Surfaces instructions + syscalls.
4. **Multi-process** — the crux for most entrypoints (shell scripts, supervisors,
   forking servers): `fork`/`execve`/`wait4`/`pipe`/`dup2` + signals, separate
   guest address spaces. This is a real embedder subsystem (we have `clone`
   threads, not processes). Design it: multiple `Vm`s? shared page cache? fd
   inheritance? Propose the model.
5. **Sockets / networking** — `socket`/`connect`/`epoll`/`poll`/`eventfd`, loopback
   or a stubbed/virtual net. Unlocks servers.

Each rung: what instructions/syscalls it likely needs, how gaps get logged and
filled, and the acceptance image.

## Invariants any of this must preserve

- **interp == JIT == Unicorn** on every instruction added (differential/fuzz/corpus).
- **Core stays guest-agnostic** — no OCI/OS/process code in core, ever.
- **Blocks(n) preemption, SMC, M7 threading, tiering** all still hold (we just
  shipped fast-dispatch R1–R6 + opt-in hotness tiering — the image runner is a
  perfect tiering client: compile-heavy one-shots).
- **Reproducible & offline first** — no network in the MVP.

## What we already have (reuse, don't reinvent)

- ELF loader (`x86jit-elf`: static + dynamic PIE, auxv, TLS).
- Syscall shim (`x86jit-tests/src/syscall.rs`): ~100 syscalls incl. file I/O with
  writable passthrough, mmap/brk, stat family, clone/futex threads, dup/dup2.
  Must be **promoted out of the test crate** into an embedder crate.
- Three-way harness (native subprocess / interp / JIT) + differential oracles.
- `x86jit-bench` per-commit native/interp/JIT timing; the image runner should plug
  into it as workloads.
- Rich SSE/SSE2 + partial SSE4/SSSE3 instruction coverage; strong dynamic-glibc
  support already proven (CPython, sqlite, lua, gzip, djpeg run three ways).

## Deliverable requested from Fable

A phased plan with:
1. **Crate architecture** — exact new crates, their responsibilities, dependency
   directions, and *precisely* what may/may-not touch core. A diagram of the
   boundary. How the syscall shim graduates from the test crate.
2. **The compatibility-map system** — its data model, how it's derived from real
   coverage (not hand-kept), CI enforcement, CPUID-consistency, and how it gates
   image selection. This is a headline deliverable.
3. **Sequenced rungs** (phases), each independently landable + testable, each
   naming likely instruction/syscall gaps, the acceptance image, and the engine
   work (esp. the multi-process subsystem design).
4. **The instruction-adding pipeline** — the repeatable process for "image hit an
   unknown instruction → add it" (decode → lift → interp → JIT → differential
   test → update the compat map), so it scales without ad-hoc drift.
5. **Structural safeguards** — how the whole thing stays professionally organized
   and does not collapse: where gaps are logged, how coverage is tracked, milestone
   structure, naming, and the smallest correct first task with its acceptance test.
6. Recommend whether this runs on `main` or a dedicated branch, and how it
   sequences against the deferred FD-AOT track.
