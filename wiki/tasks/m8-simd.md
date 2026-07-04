# M8+ — SIMD (SSE/AVX) and string ops

**Goal:** vector instruction support. Large, self-contained chapter. Real games need it; nothing on the critical path to a working scalar library does.

**Spec:** spec.md §3.1 (XMM/YMM), §12 (M8+); testing.md §10 (later). **Prereq:** M4+. Reach.

## Tasks

- [x] **M8-T1** — Add `xmm: [u128; 16]` (and YMM later) to `CpuState`; update the `#[repr(C)]` offset contract and the test `CpuSnapshot`. (§3.1, T§2)
- [x] **M8-T2** — Lift SSE/SSE2 instructions to IR and interpret them: data movement (movdqu/a, movaps/upd, movd/q, movss/sd), logic (pxor/pand/por/pandn + ps/pd aliases), packed integer arithmetic + shifts, shuffles/pack (pshufd, punpckl\*, packuswb, pinsrw), and scalar+packed float (add/sub/mul/div/min/max, sqrt, cvt\*, ucomis\*/comis\*). AVX (VEX/YMM) is a later chapter. (§12 M8+)
- [x] **M8-T3** — Codegen those vector ops in Cranelift (native float/int vector types); every one validated interp == JIT == Unicorn, plus a vectorized SHA-256 and a Newton float program run three ways. (§8.2.3)
- [ ] **M8-T4** — MXCSR / vector flag semantics (rounding-mode control, FP exception flags). Not yet demanded — current programs use default rounding; convert-to-int saturates (x86 integer-indefinite deferred). (T§10)
- [x] **M8-T5** — String ops (`rep` prefixes, DF direction flag). (T§10)

## Acceptance

- [x] **M8-T6** — Vector-instruction vectors: JIT == interpreter == Unicorn on the SIMD corpus (packed int/float, shuffles, string ops). MXCSR-affecting cases pending M8-T4. (T§8.1, T§10)

## Exit criteria

The engine runs SSE/AVX and string workloads. Extend the `vectors/` checklist (testing.md §10 "Later") with SIMD categories as coverage grows.
