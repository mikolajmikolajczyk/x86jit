---
id: TASK-284
title: >-
  perf(codegen): flag computation masks to operand size with a pooled 64-bit
  constant instead of a subregister
status: Done
assignee: []
created_date: '2026-07-22 15:11'
updated_date: '2026-07-22 15:21'
labels:
  - perf
  - 'crate:cranelift'
dependencies: []
priority: high
ordinal: 314000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measured under task-282. Disassembly of a lifted `add eax,ebx ; add eax,ebx ; jnz +2 ; hlt` block (density_tests::dump_one_shape, CHAIN=1) shows every flag computation masking the result to the operand size with a 64-bit `andq` against a CONSTANT-POOL entry rather than using the natural subregister.

Emitted today for a 32-bit operand:

    ZF:  movq %r11,%rax ; andq %rax,const(1) ; testq %rax,%rax ; setz  ->  4 insts + a pool access
    SF:  andq %r11,const(0) ; testq %r11,%r11 ; setnz               ->  3 insts + a pool access
    OF:  ... andq %rcx,const(0) ; testq ; setnz                     ->  same pool access again

`const(0)` / `const(1)` are constant-pool references: 0x8000000000000000 (sign bit) and 0xffffffff (size mask) do not fit a sign-extended imm32, so Cranelift materializes them from memory. Correct output would be `testl %r11d,%r11d ; setz` for ZF (2 insts, no pool) and a `shr`-based test or a 32-bit `test` for SF.

Root cause is in the codegen helpers, not in Cranelift: `sign_bit(size)` (x86jit-cranelift/src/codegen/mod.rs) returns a 64-bit mask constant and the callers `band` with it, instead of `ireduce`-ing the value to the operand type and letting the backend select the subregister form. Same shape appears in store_flags' inputs and in `fn parity`.

WHY IT MATTERS. task-282 established the embedder's workload (unemups4/Celeste) is FRONTEND-bound: IPC 1.02, 51% frontend stalls, 0.94 iTLB misses per kilo-instruction, flat profile across 58,599 blocks. Host instruction count and code footprint are static properties that compose, unlike latency — so cutting emitted instructions per block attacks the measured binding constraint directly. Flag materialization is 35 of the ~62 host instructions a real block emits (2.9 guest instructions per block on that title).

This is the cheapest of the three flag levers and is purely mechanical: it changes how a value is narrowed, not what is computed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 ZF/SF/OF/CF/PF computation narrows the result with ireduce to the operand type instead of band with a pooled 64-bit mask
- [ ] #2 density_tests::host_instructions_per_guest_instruction records the before/after chain-fixed cost for 'alu reg,reg'; the reduction is stated as a number in the implementation notes
- [ ] #3 no const-pool reference remains in the emitted flag code for 8/16/32-bit operands (checked by dumping the shape)
- [ ] #4 jit == interp preserved: cargo nextest run green including the differential and fuzz-seed suites
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-22.

Two changes in x86jit-cranelift/src/codegen/mod.rs:

1. `mask(v, 4)` now emits `ireduce(I32)` + `uextend(I64)` instead of `band_imm(0xffff_ffff)`.
   0xffff_ffff does not fit a sign-extended imm32, so the old form loaded it from the constant
   pool; the new one lowers to a single `movl` (x86) / `uxtw` (aarch64). Sizes 1 and 2 keep
   `band_imm` — 0xff and 0xffff are immediate-encodable and already cost one instruction.

2. New `msb(v, size) -> I8` replaces all 13 `band_imm(v, sign_bit(size))` + `icmp_imm(NotEqual, 0)`
   pairs (SF, OF, and the CF/OF sign-bit reads in shifts, rotates and double-shifts). A
   `ushr_imm` by `size*8-1` plus a `band_imm(1)` isolates the same bit with no constant at all.
   `v` need not be pre-masked: the trailing band drops anything above the sign bit, which is what
   the unmasked `of_and` operand in emit_addsub needs. `sign_bit` had no remaining caller and was
   removed.

MEASURED (density_tests, `alu reg,reg`):

                 chain fixed   marginal
    before          56.0         3.0
    after           50.7         2.6      -9.5% fixed, -13% marginal

AC#3: `grep -c 'const('` over the dumped `alu reg,reg` block is 0 — no constant-pool reference
remains. ZF also improved incidentally: `movq; andq const; testq; setz` became `testq %rdx,%rdx;
setz`, because the result is now narrowed by movl and the redundant 64-bit mask is gone.

Per-flag host instructions after the change: CF 4, PF 8, AF 7, ZF 3, SF 4, OF 7.
PF and AF are now 15 of ~33 — TASK-285 takes them next.

Worth more on aarch64 than on x86-64 (memory: x86jit-target-arm-primary): there a 64-bit mask is
not just a pool load but a multi-instruction `movz/movk` sequence when it is not encodable as a
logical immediate. Not separately measured — the density harness compiles for the host ISA only.

Gates: cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' 900/900;
clippy --all-targets --all-features -D warnings clean; fmt --check clean;
cargo check --target aarch64-unknown-linux-gnu --tests clean.

Also in this change: `density_tests::dump_one_shape` honours CHAIN=1 to terminate the shape with a
real two-way chained exit instead of `hlt`, which is what made the constant-pool loads visible.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
