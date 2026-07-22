---
id: TASK-285
title: >-
  perf(codegen): defer AF and PF — 14 host instructions per block for flags only
  pushfq can observe
status: Done
assignee: []
created_date: '2026-07-22 15:11'
updated_date: '2026-07-22 16:02'
labels:
  - perf
  - 'crate:cranelift'
dependencies:
  - TASK-284
priority: high
ordinal: 315000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measured under task-282. Per-flag cost in a lifted block (density_tests, chained exit, 32-bit `add`):

    CF 4   PF 7   AF 7   ZF 5   SF 4   OF 8   = 35 host instructions

PF and AF are 14 of those 35. Neither is read by anything that runs hot.

READERS OF AF AND PF IN THE WHOLE CODEGEN (grep 'offsets.af|offsets.pf'):
  - assemble_rflags (x86jit-cranelift/src/codegen/control.rs:168-169) — pushfq / lahf / syscall
  - eval_cond(Cond::Parity) — jp/jnp/setp
AF has NO Cond at all; x86 has no conditional branch on AF. So AF is written on every flag-setting instruction and can only ever be observed by pushfq/lahf.

What they cost today:

    PF: and $255 ; popcnt ; and $1 ; test ; setz ; movb        (7, and a popcnt)
    AF: and $15 ; and $15 ; lea ; and $16 ; test ; setnz ; movb (7)

PROPOSAL — store the SOURCE, compute at read time. Both are pure functions of values already live at the store site:

    PF = parity(res & 0xff)          -> store res's low byte:      movb %res_l, pf_src(%rdi)      (1 inst)
    AF = ((a ^ b ^ res) >> 4) & 1    -> store (a^b^res) low byte:  xor ; xor ; movb               (3 inst)

14 -> 4 emitted instructions per block. assemble_rflags and eval_cond(Parity) do the popcnt / shift-and-test on the rare read path. Both new fields are one byte in CpuState next to the existing flag bytes.

CARE REQUIRED:
  - Ops that FORCE a flag value rather than deriving it (logic ops force CF=OF=0; vector compares store constants — see the store_flag(offsets.af, z8) sites in vector.rs) must write a source byte that reproduces the forced value, not the derived one. Simplest is to keep a store of a synthesized source (AF source 0 gives AF=0).
  - The interpreter is the oracle and must stay bit-identical. Either keep the interpreter materializing AF/PF as today and make only the JIT defer them (state must then agree at every jit-vs-interp comparison point, i.e. the deferred form has to be materialized whenever state is exported), or defer in both. Decide and record which, because it decides whether CpuState's public shape changes.
  - popcntq in the current PF sequence implies a popcnt-capable host. Check the aarch64 lowering too — the primary target is x86-on-ARM (memory: x86jit-target-arm-primary), where popcount of a GPR is a multi-instruction NEON round trip and this change is worth MORE, not less.

WHY IT MATTERS AND WHY IT IS SEQUENCED FIRST. task-282 established the workload is FRONTEND-bound (IPC 1.02, 51% frontend stalls, flat profile over 58,599 blocks). Emitted code size composes; latency does not. This change plus TASK-284 removes roughly 16-25% of a block's hot instructions for a few days of local, reversible work. It is deliberately the CHEAP FALSIFIABLE PROBE for the whole direction: if a measured ~20% cut in hot code does not move the embedder's fps, the 'frontend = hot code size' model is wrong and full lazy flags (TASK-286) must not be started.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 AF and PF are stored as source bytes at the store site, not as computed flag bytes
- [ ] #2 assemble_rflags and eval_cond(Cond::Parity) derive the architectural flag values from the stored sources
- [ ] #3 flag-forcing sites (logic ops, vector compares in vector.rs) write sources that reproduce the forced value
- [ ] #4 the jit-vs-interp contract is preserved and the chosen approach (JIT-only deferral vs both engines) is recorded with its reason
- [ ] #5 density_tests records the before/after chain-fixed cost; the reduction is stated as a number
- [ ] #6 cargo nextest run green including differential + fuzz seeds; aarch64 cross-check clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-22.

REPRESENTATION. `Flags.pf: bool` -> `Flags.pf_src: u8`, `Flags.af: bool` -> `Flags.af_src: u8`.
Same width, so NO `CpuState` offset moved and `jit_abi::cpu_offsets()` is byte-identical apart
from the two renamed fields.

    pf_src = low byte of the result;   pf() = even parity of it
    af_src = a ^ b ^ result;           af() = bit 4

The `a ^ b ^ res` form gives the carry out of bit 3 for addition and the borrow into it for
subtraction, and a carry-in is already folded into `res`, so one expression covers add/sub/adc/sbb.

Accessors `pf()` / `af()` / `set_pf()` / `set_af()`, plus canonical encodings `PF_SRC_ZERO` = 1,
`PF_SRC_ONE` = 0, `AF_SRC_ONE` = 0x10, `AF_SRC_ZERO` = 0.

`PartialEq` for `Flags` is now HAND-WRITTEN and compares derived values, because one flag value has
many source encodings (any odd-parity byte clears PF) — a byte-wise derive would report two
architecturally identical machines as different. `Debug` prints derived flags for the same reason.
`Default` cannot be derived either: all-flags-clear needs `pf_src` = 1, not 0.

AC#4 — WHICH ENGINE MOVES. Only the representation moved; the INTERPRETER'S LOGIC IS UNCHANGED. It
still computes PF and AF exactly as before and writes them through `set_pf`/`set_af`. Chosen because
the interpreter is the oracle: leaving its computation untouched means any divergence the
differential suite reports points at the JIT, not at a simultaneous change on both sides. Interpreter
speed is not a goal, so there is nothing to win there anyway. The derived `PartialEq` above is what
lets the two engines hold the invariant while the JIT stores sources and the interpreter stores
canonical encodings of computed bools.

THE TRAP THE COMPILER CANNOT CATCH. Both old and new fields are one byte, so every existing
`store_flag(offsets.pf, some_i8)` still type-checks while silently changing meaning: a stored ZERO
used to mean PF=0 and now means PF=**1**. Every site was audited:

  - forced PF=0 (popcnt, ptest/vptest/vtestps, pcmpstr, kortest) -> `pf_src_const(false)`
  - `comiss`/`ucomiss`, PF = unordered -> `pf_src_from_bool(un)` (an xor, since even parity means 1)
  - imul's CF_OF-only mask passed a raw zero8 for PF -> `pf_src_const(false)`
  - AF forced to 0 -> a raw zero is already correct (`AF_SRC_ZERO` == 0)

`CpuOffsets.pf`/`.af` were renamed to `.pf_src`/`.af_src` specifically so that a future store of a
bool to those offsets reads wrong at the call site.

READ SIDE. `eval_cond(Cond::Parity)` and `assemble_rflags` (syscall/pushf/lahf) now derive via
`load_pf()` / `load_af()`. This is the cold half of the trade and is where the popcnt moved to.

MEASURED (density_tests, `alu reg,reg`, chain-fixed host instructions):

    baseline           56.0
    after TASK-284     50.7   -9.5%
    after TASK-285     40.3  -20.5%   (-28% cumulative)

Marginal per-instruction cost moved 2.6 -> 3.2 (the af_src xor chain is per-instruction and folds
less well than the old code), so at the embedder's 2.9 guest instructions per block the block goes
from ~62 to ~47 host instructions, about -25%.

Gates: cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' 900/900 — this includes
the native/Unicorn differential, which validates AF and PF against real hardware. clippy
--all-targets --all-features -D warnings clean; fmt --check clean; cargo check --target
aarch64-unknown-linux-gnu --tests clean.

NEXT: this and TASK-284 together are the cheap falsifiable probe for the direction. -28% of a
block's emitted code. If the embedder measures no fps change, the 'frontend-bound = hot code size'
model is wrong and TASK-286 (full lazy flags) must not be started.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
