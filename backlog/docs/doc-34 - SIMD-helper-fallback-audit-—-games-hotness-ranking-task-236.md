---
id: doc-34
title: SIMD helper-fallback audit — games-hotness ranking (task-236)
type: other
created_date: '2026-07-13 08:18'
---

# SIMD helper-fallback audit — games-hotness ranking

**Task:** task-236 (ps4-perf / doc-33 Tier-1). **Method:** two cross-verified
inventory passes over `x86jit-cranelift/src/codegen/{mod.rs,vector.rs}`,
`x86jit-core/src/lift/`, `x86jit-core/src/interp/`, and `backlog/docs/compat/coverage.json`.
Each SIMD op classified as **NATIVE** (`builder.ins().*` → host NEON/SSE) vs
**HELPER** (`call_helper` → per-instruction C-ABI call into the interpreter) vs
**MISSING** (not lifted at all → traps). Analysis only, no behavior change.

## Headline (this reshapes doc-33 Tier-1 / task-237)

1. **The game-hot float core is ALREADY native.** Packed/scalar
   `add/sub/mul/div/min/max/sqrt/cmp` (ss/sd/ps/pd), *all* packed-integer arith,
   *all* bitwise incl. `vpternlog`, imm-count shifts, and the common
   shuffles/blends/broadcasts/`pshufb`/`palignr` lower to `builder.ins()` → real
   NEON on ARM. **task-237 as written ("native-lower vmulps/vaddps/… , expect
   2–10×") is largely a no-op — there is no float-arith lever left to pull.** The
   task-235 `simd` microbench confirms it: its region win was only 1.1× because the
   inner `mulps`/`addps` were already native. **Reset the 2–10× expectation.**

2. **The single highest-value SIMD gap is a correctness hole, not a lowering.**
   **Packed float↔int converts** — `cvtps2dq / cvtdq2ps / cvttps2dq / cvtps2pd /
   cvtpd2ps` (+ `cvtdq2pd / cvtpd2dq / cvttpd2dq`) — are **unimplemented and TRAP**.
   They sit in the **x86-64-*v1* (baseline SSE2) `missing` list** of coverage.json —
   i.e. every x86-64 CPU including PS4's Jaguar has them, and they are ubiquitous in
   game code (per-vertex int↔float, colour/normal packing, fixed-point). A game
   hitting one traps as an unknown instruction *today*. **This is both hot and
   broken → do it first.** Native-lowerable directly: vector `fcvt_to_sint_sat` /
   `fcvt_from_sint` / `fpromote` / `fdemote` (the scalar `cvtsi2ss` etc. already use
   these — `vector.rs:2914-3016`).

3. **PS4 = AMD Jaguar → SSE4.2 + AVX-128, but NO AVX2, NO FMA3, NO AVX-512.** So
   most of the *remaining* HELPER ops (variable shifts `vpsllvd`, cross-lane
   permutes `vpermd/vpermt2`, all EVEX-masked families, AVX-512 lane ops) **cannot
   be emitted by a PS4 guest at all** — zero PS4 relevance. Even **FMA (`vfmadd*`),
   the one clearly hot helper-backed op, is never emitted by Jaguar code.** FMA is a
   real win for *other* (AVX2+) guests, but it does **not** belong on the ps4-perf
   critical path. Rank accordingly below.

## What's already NATIVE (no work — reference)

`builder.ins()` lowering, maps to NEON. (`M` = mod.rs, `V` = vector.rs line refs.)

- **Float arith:** `add/sub/mul/div ss·sd·ps·pd` → `fadd/fsub/fmul/fdiv` (M:2867-2870);
  `min/max` → `fcmp`+`bitselect` (x86 NaN/equal semantics, not IEEE `fmin`) (M:2871-2887);
  `sqrt*` → `sqrt` (M:2857).
- **Float compare:** `cmpps/pd/ss/sd` → `fcmp`+bit-negate (V:2851-2912); `[u]comis*`
  → flags (V:2818).
- **Scalar converts:** `cvtsi2ss/sd`, `cvt[t]ss2si/sd2si`, `cvtss2sd/cvtsd2ss` →
  `fcvt_*`/`fpromote`/`fdemote` (V:2914-3016).
- **Bitwise:** `and/or/xor/andn ps·pd` + `pand/por/pxor/pandn` → `band/bor/bxor`
  (M:2650-2656); `vpternlogd/q` → 8-term SOP (M:2744).
- **Packed int arith:** `padd/psub/pcmpeq/pcmpgt/pmin·pmax(u/s)/pmull(w/d)/pmulhw/
  pmulhuw/pmuludq/pmuldq/padds·psubs(u)/pavg` → native (M:2766-2852); plus
  `pmovsx/zx`, `psign`, `ptest`, `popcnt`, `pmovmskb`, `pextr/pinsr`, `movd/movq`.
- **Imm-count shifts:** `psll/psrl/psra w·d·q`, `pslldq/psrldq` → `ishl/ushr/sshr`
  (M:2598, V:1960).
- **Shuffle/blend/broadcast:** `pshufd/pshuflw/pshufhw/shufps/shufpd/unpck*/vpblendw/d/
  blendvps·pd·pblendvb/pshufb/vpbroadcastb·w·d·q/vpermq/vpermd/vperm2i128/palignr/
  insertps/roundps·pd` → `shuffle/swizzle/splat/bitselect/…` (V:360-2748, M:2364-2713).
- **Opmask (k-regs), movemask, movhlps/lhps/hps/lps/ss/sd** — native.

## Ranked worklist — hot-first (currently HELPER or MISSING)

Legend — **PS4?** = emitted by Jaguar (SSE4.2/AVX-128) code. **Verdict:** how hard to
make native. Ordered by games-hotness × reachability.

| # | Op(s) | Gen | PS4? | State | Games-hotness | Verdict | Notes |
|---|-------|-----|------|-------|---------------|---------|-------|
| 1 | **cvtps2dq / cvtdq2ps / cvttps2dq / cvtps2pd / cvtpd2ps** (+dq2pd/pd2dq/tpd2dq) | v1 (SSE2) | **yes** | **MISSING (traps)** | **very high** | **native, EASY** | vector `fcvt_to_sint_sat`/`fcvt_from_sint`/`fpromote`/`fdemote`; scalar forms already do this. **Hot AND broken → do first.** |
| 2 | **shift_reg**: `psll/psrl/psra {w,d,q} xmm, xmm` (scalar xmm count) | v1 (SSE2) | **yes** | HELPER (`shift_reg`, V:772) | medium | native, EASY–MED | broadcast lane-0 count → vector `ishl/ushr/sshr`; clamp over-shift like the imm path. |
| 3 | **dpps / dppd** | SSE4.1 | **yes** | HELPER (`dpps`, V:432/446) | medium (dot/lighting) | native, MEDIUM | `fmul` + imm src-mask + horizontal `faddv` + imm dst-mask. No single NEON op but a fixed sequence. |
| 4 | **pmaddwd** | v1 (SSE2) | **yes** | HELPER (`pmaddwd`, V:2458) | low–med (audio/codec) | native, HARD | i16×i16 → adjacent-pair i32 hadd; needs deinterleave (`swiden`+`iadd_pairwise`-style). |
| 5 | **packsswb / packssdw / packuswb / packusdw** (wide) | v1 (SSE2) | **yes** | HELPER (`vpack`, V:2446) | low–med (format) | native, HARD | interleaved saturating pack; 128-bit `packuswb` already native via clamp+shuffle — extend that. |
| 6 | **vfmadd/vfmsub/vfnmadd/vfnmsub {132,213,231}{ss,sd,ps,pd}** | FMA3 | **NO** | HELPER (`fma`/`fma_mem`, V:2078/2122) | high (off-PS4) | native, EASY | `ins().fma` (single-rounding). **Big lever for AVX2+ guests; irrelevant to PS4.** Do for the general track, not ps4-perf. |
| 7 | `vpsllvd/vpsrlvd/vpsravd/…` (per-lane variable) | AVX2 | **NO** | HELPER (`var_shift`, V:741) | med (off-PS4) | native, EASY | vector `ushr/sshr/ishl` with per-lane counts. |
| 8 | `vpermd/vpermps/vpermq(var)/vpermt2*/vshufi32x4/vperm2f128-wide` | AVX2/512 | **NO** | HELPER (permute family) | med (off-PS4) | native, HARD | cross-lane gather; some (`vpermd`) already native, rest need spill/gather. |
| 9 | EVEX-masked packed/logic/shift/blend (`{k}{z}`) | AVX-512 | **NO** | HELPER (`vmasked_*`, `vp_blendm`, …) | n/a for games | native, HARD | k-mask merge/zero over wide ALU. Deprioritize — no game guest emits AVX-512. |

### LEGIT fallbacks — keep as helpers (do not lower)

`aes*/sha*/pclmulqdq/gfni·gf2p8` (crypto), `pcmpestr[im]/pcmpistr[im]` (SSE4.2 string
state machine), `maskmovdqu`/`vmaskmov` (per-element fault suppression), `movq2dq/movdq2q`
(MMX↔XMM state bridge), `dpps`-if-deprioritized. No clean NEON equivalent; rare or
inherently sequential. Crypto could later use ARM crypto extensions, but that is a
separate, non-perf-critical track.

## Recommendations (feeds task-237 re-scope + new work)

1. **New task, top of ps4-perf: implement packed float↔int converts** (worklist #1).
   It is a *v1/SSE2 coverage hole that traps*, not a perf tweak — higher impact than
   anything in task-237. Native-lowerable, bit-exact vs unicorn, add a microbench
   (int↔float per-lane) to task-235's suite.
2. **Re-scope task-237.** Drop the "native-lower vmulps/vaddps/…" framing (already
   native) and the 2–10× claim. Retarget it at the *actually* helper-backed
   PS4-reachable ops: worklist **#2 shift_reg, #3 dpps** (both SSE, both hit real
   game code). Expect single-digit-% whole-program wins, not multiples — the hot
   float loops were never the helper cost.
3. **FMA (worklist #6) → the general (non-PS4) perf track**, not ps4-perf. Cheap
   (`ins().fma`) and high-value for AVX2+ guests, but a PS4 Jaguar guest never emits
   it.
4. **AVX2/AVX-512 helper ops (#7–#9): leave for now.** Zero PS4 reachability; only
   worth lowering if a non-Jaguar guest workload appears.

## Provenance

Native inventory: `codegen/mod.rs:512-1311` dispatch → `emit_v_*`/`emit_*` in
`vector.rs`; primitives `emit_fbin` (M:2867), `emit_packed_bin` (M:2766), `emit_vlogic`
(M:2650), `emit_pshufb` (M:2364), converts (V:2914-3016). Helper set:
`Helpers` struct `codegen/mod.rs:44-99`, wired `mod.rs:~3563-3597`, impls in
`x86jit-core/src/interp/`. MISSING converts confirmed: 0 hits in lift/interp/cranelift,
listed under `x86-64-v1/missing` in `backlog/docs/compat/coverage.json`.
