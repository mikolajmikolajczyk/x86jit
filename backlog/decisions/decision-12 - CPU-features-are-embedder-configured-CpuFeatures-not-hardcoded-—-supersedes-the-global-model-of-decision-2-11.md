---
id: decision-12
title: >-
  CPU features are embedder-configured (CpuFeatures), not hardcoded — supersedes
  the global model of decision-2/11
date: '2026-07-08 19:17'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk

## Context

`cpuid_run` hardcoded a single global feature set and `xgetbv` baked a constant
XCR0. Every advertise choice was therefore global: decision-2 dropped SSE4 because
*some* corpus glibc would then execute unlifted `pcmpistri`; decision-11 flipped
AVX/AVX2 on for *everyone* at once. The pending "advertise AVX-512" step
(task-168.5 AC#5) was framed as a risky all-or-nothing corpus verify-loop **only
because it was global** — flipping the bit switches the entire green corpus onto
EVEX paths the lifter doesn't yet cover.

That is the wrong shape for a guest-agnostic recompiler library. A hypervisor lets
the embedder declare the guest CPU (`qemu -cpu`); x86jit should too.

## Decision

The guest CPU feature set is an **embedder-selected, per-run value** (`CpuFeatures`
in `x86jit-core`, task-169). Presets `baseline`/`v2`/`v3`/`v4` plus `with`/`without`
toggles; `cpuid_run` and the now-runtime `xgetbv` project it into CPUID leaves /
XCR0. `Vm::set_cpu_features` selects it; `x86jit-cli --cpu <level>` exposes it.

**`CpuFeatures::default()` reproduces exactly what was hardcoded before** (SSE,
SSE2, MMX, SSE3, SSSE3, POPCNT, XSAVE, OSXSAVE, AVX, AVX2) — zero behavior change
for any embedder that doesn't opt in. The compat test
`cpuid_advertises_only_what_lifts` now guards the **default** preset (advertise ⊆
lift).

This **supersedes the *global* nature** of decision-2 and decision-11, not their
technical content: their rationale (why SSE4/AVX-512 stay off *by default* — the
lifter doesn't cover `pcmpistri`/masked EVEX yet) survives as the documentation of
the default preset. They are no longer *laws*; they describe one preset among
several.

## Consequences

- Advertising AVX-512 stops being a scary global flip. An AVX-512 test/run selects
  `v4`; the corpus stays on the default. task-168.5 AC#5 is rewritten accordingly.
- Advertising past the lifter's coverage is a **documented caller risk**: the guest
  traps on the unimplemented instruction (a legal `Exit`), not a library bug.
  Verified: `x86jit-cli --cpu v4 /usr/bin/true` (CachyOS v4 coreutil) clears every
  glibc CPUID level check and traps on the first unlifted EVEX op (`vpxorq`).
- The compat map gains an `x86-64-v4` generation row tracking AVX-512 lift progress.
- MMX rides in every preset (present on all x86-64; load-bearing for glibc's
  cpu-features init — the decision-2 waiver), though no MMX instruction is lifted.

## Alternatives considered

- **Keep hardcoding, flip AVX-512 globally when lifted** — keeps every advertise
  change a corpus-wide risk and blocks running v4 binaries on a single global
  decision. Rejected.
- **A build-time cargo feature per ISA level** — not per-run, can't differ between
  two VMs in one process, and leaks ISA policy into the build. Rejected.

## Trigger to revisit

If a preset's default set needs to change (e.g. the AVX-512 lift reaches parity and
`v4` becomes safe as the default), or if the advertise ⊆ lift invariant needs
per-preset waiver lists rather than the single default-preset waiver.
