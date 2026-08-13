# Provenance — where this engine's behaviour comes from

An emulator is a claim about how somebody else's hardware behaves. The claim is only
worth what its sources are worth, so this document records them: what we treat as
authoritative, what we merely compare against, what we deliberately do not model, and
what we did not copy.

The rule is the one [`oracles`](https://github.com/unemu-org/oracles) exists to serve:
**every behavioural fact must be derived from, and cite, a clean primary source, and the
citation has to be checkable by someone who was not there.** `oracles` is vendored here
as a submodule so the paths a citation names stay stable and the version is pinned by
commit:

```sh
git submodule update --init
cd oracles && ./fetch-oracles.sh verify   # offline; the vendored sources are already there
cd oracles && ./fetch-oracles.sh fetch    # adds the fetch-only ones (the Intel SDM, the APM, the psABI)
```

## 1. What is authoritative

| Source | Role | Pin |
|---|---|---|
| **Intel SDM**, combined volumes 1, 2A-2D, 3A-3D, 4 | **The** authority for instruction semantics, flag effects, the x87 environment and tag-word images, exception behaviour | Order Number `325462-092US`, June 2026, 5363 pp — `oracles/intel/sdm.pdf`, fetch-only |
| **AMD64 APM**, volumes 1-5 | **Second witness** where the SDM is silent or says "undefined". Also the natural authority for the Jaguar-class (x86-64-v2) target | Publication `40332`, revision 4.08 — `oracles/intel/amd64-apm.pdf`, fetch-only |
| **x86-64 psABI** | Process initialisation: the initial stack, the auxiliary vector, the exception→signal map, and the syscall calling convention (RDI, RSI, RDX, **R10**, R8, R9 — not RCX) | commit `e1ce0983`, LaTeX sources — `oracles/psabi/`, fetch-only |
| **Linux x86 syscall tables and process/signal uapi** | Syscall numbers for both the x86-64 and the i386 `int 0x80` tables; the `siginfo`/`sigcontext` layouts | `v6.12` — `oracles/linux-x86/`, vendored |

Cite by **volume and section**, not by "the SDM". `[SDM Vol 1 §8.1.7]` is a citation;
"per Intel" is not — the second cannot be checked, and an unfalsifiable citation is worse
than none because it looks like one.

**Where this repository stands today, stated honestly** (counted, not estimated — rerun the
count when you change this paragraph): **537** internal `§spec` references against **62**
mentions of an external authority, of which only **8** name a volume and a section, and
**one** URL across the crates' Rust sources (35 across the whole tree once docs, scripts
and manifests are included — the figure that matters here is the Rust one, because a URL
in a doc is a link while a URL in a comment is a citation standing in for a real source). The sources above are what a precise citation can now
point at; retrofitting the vague ones is standing work, not a finished state.

### Witness tests — a citation you can execute

Prose citations rot silently: nobody notices when a comment and the code drift apart. So a
cited constant is also **pinned by value in a test**, which makes the claim falsifiable by
anyone, with **no stash and no network** — the manuals are for deriving *new* facts and for
human verification, never a build dependency.

`features.rs::cpuid_feature_bits_match_the_documented_positions` is the worked example. It
restates the CPUID feature-bit tables from SDM Vol 2A and asserts that a set containing
exactly one feature projects to exactly the documented bit. Trusting the `if_has(f, n)`
calls would be circular — the test would pass whatever the code said. Instead, moving AVX
from bit 28 to 27 fails it by name. That matters because advertising a bit we do not lift
surfaces as "glibc picked a string routine that traps", a very long way from its cause.

The same shape appears wherever a value comes from a document rather than from reasoning:
`x87::rc` (control-word bits 11:10, SDM Vol 1 §4.8.4) is witnessed by a test that `fistp`s
`(0.75, -0.75)` under each of the four rounding modes, a pair chosen because it separates
all four — a mis-decoded field cannot pass it.

A citation can also settle **how much to guarantee**, not just what a value is.
`x86jit-tests/tests/cross_modifying.rs` runs the two-processor protocol from
**SDM Vol 3A §11.1.3** ("Handling Self- and Cross-Modifying Code") — modifier stores the
code then raises a flag, executor polls, executes a serializing instruction, then runs it
— because that is the case the architecture actually defines. The same section calls the
*unsynchronized* form "model-specific", which is what stopped this engine from being held
to a stronger rule than the hardware offers: an acceptance criterion asking that a stale
translation can never run was replaced on the strength of that sentence (task-323). The
manual is as useful for bounding an obligation as for supplying a constant.

## 2. What is an oracle — and what each one can actually judge

An oracle is something we compare against. **None of them is an authority.** The
distinction is load-bearing here, because this engine has repeatedly been right where an
oracle was wrong.

| Oracle | Judges | Cannot judge |
|---|---|---|
| **Real host CPU** (`x86jit-tests/src/native.rs`) — forks, runs the snippet on bare hardware, captures state from a signal handler | Anything the silicon does, including VEX/EVEX semantics | x87 registers (not captured); needs an x86-64 host, so it is absent on the ARM lane |
| **Unicorn** (QEMU TCG), behind the `unicorn` feature | Broad cross-platform agreement, and the only oracle available on ARM | Anything QEMU gets wrong — see below. Its build predates TCG AVX |
| **The interpreter** | The JIT (`jit == interp` is the JIT's oracle) | Nothing, when both tiers share a bug — which is why neither is the ground truth |
| **SingleStepTests 80286** — captured from a real Harris N80C286-12 through ArduinoX86 | Real-mode per-instruction behaviour, from silicon | Only the 80286 subset; pinned at `oracles/ss286`, corpus fetched separately |
| **objdump** | Our disassembly against binutils | Semantics |

**Unicorn is a comparand, not an authority.** The tree records around twenty places where
QEMU is wrong and hardware agrees with us. Three worked examples, all measured rather
than argued:

- `fnstenv` writes the reserved half-words as `0xFFFF`; QEMU writes zero. Silicon agrees
  with us.
- `fnstenv` masks all six FP exceptions after the store, per the SDM. QEMU omits the side
  effect entirely.
- On an MMX write, Intel sets the x87 exponent bytes to all-ones; Unicorn leaves them
  zero.

Where an oracle disagrees, the SDM settles it, and the divergence is written down with the
measured bytes — never quietly excluded.

## 3. What we do not model, and say so

Refusing to lift is a design position, not an omission, when the alternative is a
plausible-looking wrong answer:

- **x87 `FIP`/`CS+FOP`/`FDP`/`FDS`** are not modelled: no instruction updates them. They
  are carried verbatim across `fldenv`/`fnstenv`, so a `fenv_t` save/restore round trip is
  exact, but the values are whatever the guest last loaded rather than the last
  instruction's. `fnsave`/`frstor` remain **unlifted**.
- **The six FP exception flags ARE set** by arithmetic since task-328, and each rule is
  witnessed against the host rather than restated from the manual — including the two that
  are easy to get backwards: masked underflow needs the result to be both tiny and
  inexact, and ES follows the *masks*, not the exception. What is still missing is
  delivery: **no `#MF` is ever raised**, so a guest that unmasks an exception gets ES set
  and a result rather than a trap. The stack-fault flag and the condition codes C0/C2/C3
  are also unmodelled, which is why `ficom`/`ficomp` stay unlifted while the rest of the
  x87 integer-arithmetic family is implemented; `fcomi`/`fucomi` work because they write
  EFLAGS instead. A denormal operand does not raise DE — `F80::from_bytes` folds denormals
  into the normal class, so the fact is gone before arithmetic sees it. All `TASK-328`.
- **MXCSR** is not modelled as behaviour: `stmxcsr` stores the reset value `0x1F80` and
  `ldmxcsr` is a no-op. It *is* measured — the native oracle captures the real register
  and the comparator compares its control half, so a guest that changes rounding control
  and reaches that leg is reported rather than silently accommodated. The six sticky
  exception flags are captured but not compared; `deferred.md` says why.

## 4. What we did not copy

No third-party source is vendored into this tree, and no file carries a foreign copyright
or SPDX header — checked, not assumed. Nothing here is a port of another emulator's
implementation. Where another project is named in a comment it is in one of two roles:
*this is what mature translators do* (the memory-ordering design cites Box64's `STRONGMEM`
levels and FEX's RCpc use as precedent), or *this oracle is wrong here*.

`x86jit-core`'s dependency set is exactly `{iced-x86}` — the x86 decoder, the one thing a
recompiler legitimately needs — and `x86jit-tests/tests/boundary.rs` fails the build if
that ever changes. That tripwire is why extracting the Linux userland into `unemulinux`
was a move rather than an untangling.

## 5. Licence surface

This project is `MIT OR Apache-2.0`. The dependency graph, by crate:

| Crate | Direct dependencies | Licence |
|---|---|---|
| `x86jit-core` | `iced-x86` | MIT |
| `x86jit-cranelift` | `x86jit-core`, `cranelift{,-jit,-module,-native}`, `memmap2` | Apache-2.0 WITH LLVM-exception; MIT OR Apache-2.0 |
| `x86jit-elf` | `x86jit-core`, `goblin` | MIT |
| `x86jit-tests` | `serde`, `ron`, `hex`, `serde_json`, `flate2`, `iced-x86`, `libc`, **`unicorn-engine`** | permissive, except as below |
| `x86jit-bench` | `serde`, `serde_json`, `iced-x86` | permissive |

**`unicorn-engine` is GPL-2.0**, and this needs stating plainly because `spec.md §15` said
for a long time that "all core dependencies are permissive", which stopped being true when
the Unicorn oracle was added. It is:

- **optional**, behind the `unicorn` feature, off by default;
- confined to `x86jit-tests`, which is `publish = false` and which nothing else in the
  workspace depends on at build time;
- dynamically linked against the system `libunicorn.so` rather than statically built in;
- **enabled in CI**, so the default CI test binary does link it.

Nothing shipped from this repository links it. A consumer of `x86jit-core`,
`x86jit-cranelift` or `x86jit-elf` never sees it.

The one committed guest binary, `x86jit-tests/programs/pthreads.elf`, is our own work.
Every third-party guest binary left with the Linux userland when `unemulinux` was split
out — which is also how this repository stopped shipping busybox (GPL-2.0), the glibc
loader (LGPL-2.1), and lua and CPython, both of which statically link GNU Readline and
are therefore GPL-3.0 combined works.

## 6. A clean source can still be second-hand

`oracles` documents a case where one file in an otherwise-clean upstream tree cited
another emulator as the origin of its own constants; its fetcher excises that block and
re-checks it on every verify. The general lesson holds here too: **"it came from the
stash" is not by itself proof that a fact is first-hand.** Read
`oracles/MANIFEST.md` § *Second-hand blocks in a clean source* before leaning on a
citation.

This bit us concretely, and the fix is worth recording. The 80286 corpus fetcher
(`x86jit-tests/vendor/80286/fetch.sh`) used to pull from a moving `main` with no checksum,
and never downloaded `revocation_list.txt` — which lives at the repository root rather than
under `v1_real_mode/` — so every test the corpus author had marked bad was silently trusted,
because the loader reads an absent file as an empty list. The fetcher now pins the same
commit `oracles` records and downloads the revocation list; CI fetches the corpus and sets
`SS286_REQUIRED=1`, so its absence fails instead of skipping.
