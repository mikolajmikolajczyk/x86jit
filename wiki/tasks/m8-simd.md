# M8+ — SIMD (SSE/AVX) and string ops

**Goal:** vector instruction support. Large, self-contained chapter. Real games need it; nothing on the critical path to a working scalar library does.

**Spec:** spec.md §3.1 (XMM/YMM), §12 (M8+); testing.md §10 (later). **Prereq:** M4+. Reach.

## Tasks

- [ ] **M8-T1** — Add `xmm: [u128; 16]` (and YMM later) to `CpuState`; update the `#[repr(C)]` offset contract and the test `CpuSnapshot`. (§3.1, T§2)
- [ ] **M8-T2** — Lift SSE/AVX instructions to IR (new vector `IrOp`s / value widths); interpret them. (§12 M8+)
- [ ] **M8-T3** — Codegen vector ops in Cranelift; validate against the interpreter oracle. (§8.2.3)
- [ ] **M8-T4** — MXCSR / vector flag semantics as needed. (T§10)
- [ ] **M8-T5** — String ops (`rep` prefixes, DF direction flag). (T§10)

## Acceptance

- **M8-T6** — Vector-instruction vectors: JIT == interpreter == Unicorn on the SIMD corpus, including MXCSR-affecting cases. (T§8.1, T§10)

## Exit criteria

The engine runs SSE/AVX and string workloads. Extend the `vectors/` checklist (testing.md §10 "Later") with SIMD categories as coverage grows.
