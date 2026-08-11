# Ready-to-file upstream report

Not filed. Posting to an external tracker is the maintainer's call; everything needed
is below. Target: `llvm/llvm-project` issues, labels `backend:X86`, `miscompilation`,
`llvm:SelectionDAG`. Re-run `./run.sh` before posting — it exits 1 if the toolchain has
been fixed, which would make the report moot.

---

**Title:** [X86] i128 select-of-constants mask miscompiled: high 64 bits of the result
are dropped (regression in 21.1)

**Body:**

`llc` drops bits 127:64 when an `i128` value is masked with a `select` between two
constants and the other operand of the merge was loaded in a predecessor block. The low
half is correct; the high half comes out zero.

Reduced module: [`v_sel_pred.ll`](v_sel_pred.ll) (attach; 40 lines, no target
intrinsics). Driver: [`drv.c`](drv.c). Both are in this directory along with three
near-miss variants that isolate the trigger.

```
$ llc -O2 -filetype=obj -relocation-model=pic -o v.o v_sel_pred.ll
$ cc -O0 -o v drv.c v.o && ./v
want         hi=aaaaaaaaaaaaaaaa lo=bbbbbbbb3fb6cd8e
got          hi=0000000000000000 lo=bbbbbbbb3fb6cd8e
MISCOMPILED
```

**LLVM disagrees with itself on the same module**, which is what makes this a codegen
bug rather than an ambiguity in the IR:

```
lli --force-interpreter   hi=aaaaaaaaaaaaaaaa lo=bbbbbbbb3fb6cd8e   (correct)
lli (JIT, X86 backend)    hi=0000000000000000 lo=bbbbbbbb3fb6cd8e   (wrong)
```

**Affected versions** — a regression, and still present in the newest release:

| LLVM | result |
|---|---|
| 19.1.7 | correct |
| 20.1.8 | correct |
| 21.1.8 | miscompiled |
| 22.1.2 | miscompiled |
| 22.1.8 | miscompiled |

18.1.8 rejects the module, so the window is bounded below by 20.1.8 only.

**Not opt-level dependent.** `llc -O0`, `-O1`, `-O2` and `-O3` all produce the wrong
result, which points at legalization/ISel rather than an optimization pass.

**Target-specific.** The same module retargeted to `aarch64-unknown-linux-gnu` and run
under `qemu-aarch64` is correct on every variant. That leg carries a positive control —
a fifth module whose IR really does zero the high half — which does report
`MISCOMPILED`, so the ARM result is a measurement rather than a harness that cannot
fail.

**Trigger** needs both halves, established by the four-variant matrix in `run.sh`:

1. the mask is an `i128` `select` of two constants (a constant mask is fine), **and**
2. the other merge operand is loaded in a predecessor block (loading it in the same
   block is fine).

**Origin.** Found in an x86-64 emulator's software x87/SSE path, where the shape is
`(a & !mask) | (src & mask)` merging a 64-bit float into the low half of a 128-bit
vector register. The guest saw a register whose upper half silently kept its old
contents, which surfaced as wrong results in a real program long before anyone
suspected the compiler.
