---
id: TASK-276
title: >-
  cranelift: opt_level is never set, so all JIT codegen runs with the optimizer
  OFF
status: Done
assignee: []
created_date: '2026-07-22 05:16'
updated_date: '2026-07-22 06:35'
labels:
  - cranelift
  - perf
dependencies: []
priority: high
ordinal: 306000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
x86jit-cranelift/src/lib.rs:1950 and x86jit-cranelift/src/codegen/mod.rs:3885 both build a settings::builder() and set only use_colocated_libcalls / is_pic. Neither sets opt_level, so both take Cranelift's default.

That default is none. Confirmed in the vendored source rather than from memory — cranelift-codegen 0.115.1 src/settings.rs:513 dumps opt_level = none for a default builder, and its own test at :551 asserts f.opt_level() == OptLevel::None. The accepted values are none, speed, speed_and_size.

So every block and region we compile is emitted with Cranelift's mid-end optimizer disabled: no egraph pass, hence no GVN, no LICM, no constant folding, no alias analysis, no redundant-load elimination.

Lifted x86 is unusually rich in exactly what those passes remove. Flags are recomputed and then overwritten unread; the same base+displacement address arithmetic is rebuilt for every access in a block; guest register spills reload values that are provably unchanged. A lifter emits this by construction because it translates instruction by instruction, and only the optimizer can see across that boundary.

The cost is compile time, and this engine is unusually well placed to absorb it: tier-up already compiles off the vcpu on the backend worker thread (set_tier_up_background(true)), so a longer compile delays only when a block gets swapped in, not the execution meanwhile.

Work:
- set opt_level explicitly at both sites rather than relying on a default, and treat the value as a deliberate choice with a comment saying why
- measure speed and speed_and_size against none: guest throughput, compile_ns, and compiled-code size
- check the interaction with HostTarget::Baseline: that path pins off AVX/FMA for deterministic, portable codegen, so confirm the optimizer does not reintroduce a difference the Baseline mode exists to prevent (mul+add contraction is the obvious thing to check)
- verify the differential suites still pass at the chosen level — an optimizer bug shows up as a semantic difference, which is precisely what those suites exist to catch

Measured context from the unemups4 embedder (Celeste, retail title): guest x86 execution is about 62% of a gameplay frame at 22-25 fps, and slow-frame RIP sampling shows the flipping thread is 99% on-core, i.e. genuinely computing rather than blocked. Guest-code throughput is now that embedder's dominant cost, so an optimizer level change is directly measurable there. Sampling also concentrates 10.9% of slow-frame samples in a single 0x3c-byte loop, which is a good before/after target.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 opt_level is set explicitly at both settings::builder() sites, with a comment recording the chosen value and the reason
- [x] #2 speed and speed_and_size are measured against none for guest throughput, compile_ns and code size, with the numbers recorded
- [x] #3 the HostTarget::Baseline determinism guarantee still holds at the chosen level (no FMA contraction or other reintroduced host-dependent difference)
- [x] #4 the differential test suites pass at the chosen level
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented (not yet committed). Made CONFIGURABLE rather than hardcoded, per the request.

API. New `OptLevel` enum in x86jit-cranelift (None / Speed / SpeedAndSize), mirroring the existing `HostTarget` pattern — our own type, so Cranelift's does not leak into the public API. `OptLevel::parse` for the env spelling. Constructors: `with_opt_level(opt)` plus `with_options(caps, target, opt)`. Env knob `X86JIT_OPT_LEVEL=none|speed|speed_and_size` parsed at the edge in `EngineConfig::from_env` (task-181 convention: no env reads inside the library); an unrecognized value falls back to the default instead of failing the run. The bench reads the same var in its own `opt_level()` helper so all three levels are measurable.

SCOPE NOTE — one thing had to change beyond setting a flag. `EngineConfig::backend()` was a mutually-exclusive chain (`if superblocks ... else if baseline ...`), so the three JIT axes could not be combined and a third axis would have been silently dropped whenever superblocks were on — i.e. exactly the configuration the reporting embedder runs. Replaced with a single `with_options` call. Side effect: superblocks + HostTarget::Baseline now actually compose, where previously superblocks silently won.

AC#2 MEASURED (x86_64, --release, 5 iters, bench artifacts restored afterwards).
Guest run time (ms, lower better) — none -> speed:
  fib32    106.17 -> 100.32  (-5.5%)
  sha256     8.42 ->   6.38  (-24.2%)
  hotloop   44.71 ->  42.57  (-4.8%)
  simd       5.56 ->   4.62  (-16.9%)
  memcpy     7.25 ->   5.98  (-17.5%)
  indirect  56.90 ->  53.42  (-6.1%)
Every hot workload improves. Compile time rises ~45-48% (sha256 12.18 -> 17.62 ms; sqlite 1441 -> 2128 ms). Compiled code size across a whole bench run (via X86JIT_PERF_MAP=1, summing the map's size field): none 33,893,327 B / 807.5 avg; speed 31,265,166 B / 749.3 avg (-7.8%); speed_and_size 31,237,924 B / 748.2 avg. So `speed` is both faster AND smaller than `none`; `speed_and_size` buys a further 0.09% of size for no throughput. Default = Speed.

HONEST TRADE-OFF: the one-shot workloads (sqlite, lua) are compile-dominated and get WORSE overall — sqlite jit-cold 1436 -> 2102 ms. They are excluded from the perf gate (`gated = cw.kind != "one-shot"`), so this does not trip it, but a caller whose workload is compile-bound should set X86JIT_OPT_LEVEL=none. That is what the knob is for.

AC#3. Checked in the vendored cranelift-codegen 0.115.1 source rather than from memory: src/opts/ contains exactly TWO float rewrite rules, both sign cancellation — `(fmul (fneg x) (fneg y)) -> (fmul x y)` and the same for an existing `fma`. There is NO rule that creates an `fma` from `fmul`+`fadd`, so the mid-end cannot contract. Independently, Baseline pins has_fma=false at the ISA level, so lowering cannot either. baseline_host_target_lowers_guest_avx_to_sse passes.

AC#4. cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' -> 885/885 passed. Also ran the AVX/VEX differential fuzzer for 240 s at the new default.

FUZZ FINDING, PRE-EXISTING, NOW FILED AS TASK-277: the fuzzer reports a [JIT-vs-interp] divergence on vdpps (seed 26816) plus native-vs-interp on vdpps (seed 20980). Verified NOT caused by this change by stashing the whole task-276 diff and re-running seed 26816 on the pre-change tree — byte-identical divergence. Note the first comparison I ran was invalid (the fuzz binary uses JitBackend::new() and does not read X86JIT_OPT_LEVEL, so the 'none' run was really Speed); the stash re-run is the one that counts.

CAVEAT: all numbers are one x86_64 host. ARM is the primary target and is unmeasured here — the mid-end passes are host-independent, but the throughput win is not necessarily the same size there.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
