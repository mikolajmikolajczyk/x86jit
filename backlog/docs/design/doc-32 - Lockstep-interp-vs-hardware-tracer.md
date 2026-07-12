---
id: doc-32
title: Lockstep interp-vs-hardware tracer
type: guide
created_date: '2026-07-12 05:03'
---

# Lockstep interp-vs-hardware tracer

A forensic tool for the hardest class of x86jit bug: one where the interpreter and
the JIT **agree with each other** but **both disagree with a real x86-64 CPU**. Those
are invisible to the whole differential test suite (which is interp-vs-JIT-vs-Unicorn),
and they're the ones that silently corrupt a long computation — a wrong crypto result,
a mis-decoded key — with no crash to localize.

It found two such bugs in one sitting (task-215):

- **`vzeroall`** cleared only the upper lanes, leaving xmm stale — both backends lifted
  it identically wrong. Corrupted openssl's rsaz-avx2 keygen.
- **16-bit `movbe`** byte-swapped 32 bits instead of 2 in the *interpreter only* (the JIT
  was correct, but no test exercised 16-bit movbe, so interp==JIT never fired).
  Corrupted openssl's PEM/base64 key decode → invalid RSA signatures.

Both were caught by replaying the interpreter's real execution against the host CPU.

## When to reach for it

Use it when a whole-program run under x86jit produces a **wrong result with no trap**,
and:

- the result is **deterministic** (rules out RNG/uninitialized state), and
- **interp and JIT agree** (so the differential suite is blind to it), and
- per-op fuzzing hasn't found it (the bug is operand-specific, or on an op the fuzzer's
  menu doesn't cover — e.g. a zero-operand op like `vzeroall`).

If interp and JIT *disagree*, use the ordinary differential tests instead — they're
cheaper and pinpoint the same thing.

## How it works

Two halves.

**Capture** (`x86jit-core/src/lockstep.rs`, env-gated, zero cost when off). Hooked into
the interpreter at each `IrOp::InsnStart`. It records the **full architectural
side-state** around every instruction it runs — the 16 GP registers, arithmetic flags,
a 64-byte window at the memory operand's effective address, and xmm/ymm 0–15 — both
*before* and *after* the instruction. Consecutive `InsnStart`s bracket exactly one
instruction (a vector/data op is never the last op of a block, since a block ends *at*
control flow), so `post_i == pre_{i+1}` and no end-of-block flush is needed. Records
stream to a trace file.

**Replay** (`x86jit-tests/src/native.rs`, the `#[ignore]`d `replay_lockstep_trace`
test). For each captured record it assembles `[load pre-state; the instruction bytes;
hlt]`, runs it on the **real host CPU** via `run_native` (fork + fault handler +
XSAVE capture), and compares the hardware's post-state to the interpreter's captured
post-state. The first mismatch is the exact instruction — with the program's *real*
operands — that the interpreter computes wrong.

Because `run_native` pins fixed guest VAs, replay is parallelized across **processes**,
not threads: each shard owns the VAs in its own address space and handles record
`index % shards == shard`. The global scan index is printed at a divergence, so the
earliest bug across shards is the one with the smallest index.

## Usage

Driver: [`scripts/lockstep.sh`](../../../scripts/lockstep.sh).

```sh
# 1. Capture (MUST be --backend interp — the hook is in the interpreter).
scripts/lockstep.sh capture -- \
  ./target/release/x86jit-cli --backend interp --cpu v4 --entropy host \
  /usr/bin/openssl dgst -sha256 -sign key.pem -out /tmp/sig data.bin

# 2. Replay against the host CPU (auto-sharded across cores).
scripts/lockstep.sh replay
```

Defaults that matter: the whole program is traced (all vector + scalar/data ops, any
address), capped at 20M records so a startup-heavy program stays bounded — the first
divergence is what you want, and startup is almost always clean, so a cap that reaches
the interesting phase is enough. Traces are tens of GB and land on real disk
(`$XDG_CACHE_HOME`), not tmpfs. Narrow with `--lo/--hi` (guest address window) or
`--max` if you know where to look; widen `--max 0` for unbounded.

Underlying env knobs (if driving the harness directly): `X86JIT_LOCKSTEP=<file>`,
`X86JIT_LOCKSTEP_LO/_HI` (hex window), `X86JIT_LOCKSTEP_MAX` (record cap) on the capture
side; `X86JIT_LOCKSTEP_REPLAY=<file>`, `X86JIT_LOCKSTEP_SHARDS/_SHARD`,
`X86JIT_LOCKSTEP_FLAGS` on the replay side.

## Blind spots

A clean replay does **not** prove the traced region correct. The tracer cannot see:

- **Control flow.** Branches, calls, and rets aren't traced, and every op is replayed
  from its *own* captured pre-state — so a wrong-branch bug (the interpreter takes a
  different path but each op it runs is locally correct) is invisible. Diagnosing that
  needs branch-point instrumentation, not this.
- **Masked EVEX and segment-relative memory.** Ops with a k-register operand, or an
  FS/GS-relative memory operand, are skipped: the native stub can't establish opmask
  state or guest segment bases. AVX-512 masked crypto is therefore a gap.
- **Flags, by default.** Flag comparison is opt-in (`--flags`) and noisy: the
  interpreter elides dead flags (an op whose flags are overwritten before any read gets
  `FlagMask::NONE`), so a post-op flag snapshot legitimately differs from hardware.
  Treat any `--flags` divergence as suspect until you confirm the flag is actually live.

## The recipe that works

For a deterministic wrong-result-no-trap bug: capture the whole program (default
window, a cap that reaches the buggy phase), replay, read the first divergence. If the
region is clean, widen the window / raise the cap, or suspect a blind spot above. Both
task-215 bugs fell out of exactly this — capture-all + replay + first divergence.
