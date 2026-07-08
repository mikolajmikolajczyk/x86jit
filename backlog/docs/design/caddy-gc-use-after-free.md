---
id: doc-31
title: 'caddy corruption — GC use-after-free investigation (task-161)'
type: guide
created_date: '2026-07-08'
---

# caddy corruption — GC use-after-free investigation (task-161)

Deep-debug findings for the guest-correctness corruption that hits the **real
`caddy` binary** under the threaded **interpreter** (go-caddy track, task-161).
Recorded here so the investigation survives across sessions — it is long, the
repro is timing-fragile, and the tooling (offsets, harness) is worth keeping.

**Status (2026-07-08):** root cause **category confirmed** — the Go GC frees
**live** regexp-tree objects (a reachability / root-scan miss), producing a
use-after-free. Not yet fixed. A separate, real bug found along the way *was*
fixed and committed (task-165, see below). Everything below is evidence-backed;
negative results are recorded as deliberately as the positive ones.

---

## 1. Symptom

`caddy version` under the threaded interpreter boots the full Go runtime (GC
workers, scavenger, finalizers) and then **intermittently** dies during package
`init` with:

```
panic: regexp: Compile(`…`): error parsing regexp: expression too large: `…`
  regexp.MustCompile(...)
  github.com/yuin/goldmark/extension.init()   (linkify.go / table.go)
```

The regex is a **normal** one (goldmark linkify/table). "expression too large"
comes from `regexp/syntax.(*parser).checkSize`/`calcSize` reading an absurd
size. Earlier the same corruption surfaced as the GC's own
`fatal error: found bad pointer in Go heap`. Both are the same underlying fault
seen from two angles.

Key qualifiers:
- **Interpreter only** in practice (JIT clean). Best current understanding: the
  JIT is ~100× faster so its race window is tiny; the interp's slow per-op
  timing widens it. Treat as "interp-exposed", not necessarily interp-*caused*.
- **Multi-threaded only.** `GOMAXPROCS=1` is clean.
- **Contention-gated** (see repro): ~0% on an idle host, ~45% under CPU
  oversubscription.

---

## 2. Reliable repro (the biggest practical deliverable)

The bug was long considered "extremely timing-fragile, can't reproduce on
demand." It is actually **contention-gated** and reproduces reliably under CPU
oversubscription, as **fresh processes**:

- Spawn `3 × nproc` busy-loop stressors (`while true; do :; done &`).
- Run `caddy version` through the threaded interp **once per process** (a fresh,
  cold process each run — an in-process loop warms the reserved-arena page
  residency and *hides* it).
- Measured baseline: **~45%** corruption (e.g. 18/40) at 3× oversubscription;
  ~10–50% at 2×. Idle: **0/15**.

Why earlier probes "reproduced ~2/3": they ran under `cargo test` / `nextest`
whose **parallel compilation** supplied exactly this CPU contention.

Harness (this session, kept in the scratchpad, trivially recreatable):
- `loadtest.sh <label> <M> <mult>` — spawns `mult × nproc` stressors, runs the
  probe binary `M` times as fresh processes, counts BAD, **reaps the stressors**.
- `caddy_probe.rs` — `include_bytes!("../programs/caddy.elf")` +
  `Guest::new_static(CADDY).reserved(1<<40).heap_base(0x600_0000)
  .brk_limit(0x680_0000).mmap_base(0x1_0000_0000)
  .mmap_limit(0x1_0000_0000 + (512<<30)).stack_top(0x8000_0000)
  .argv([caddy, version]).run_threaded_full(InterpreterBackend)`.
- `Guest::run_threaded_full` — a temp variant of `run_threaded` returning
  `(stdout, stderr, exit_code)` so the guest panic on fd-2 is visible.
- Guest-env passthrough (`GUEST_GODEBUG`, `GUEST_GOGC`) for the discriminators.

None of the above is committed — the 52 MiB `caddy.elf` fixture is gitignored
and would break CI. Recreate from this doc.

> **Operational warning:** every load-run spawns `3 × nproc` busy-loops. If they
> are not reaped (an interrupted ad-hoc loop), they pile up — this session hit
> **load average 342** across ~7 leaked generations, which made everything
> crawl. Always `trap`/`kill` the stressor PIDs; verify with
> `ps -eo pcpu,comm | awk '$2=="bash" && $1>3'`.

---

## 3. Confirmed root cause: GC frees LIVE regexp nodes

The chain of confirmations:

1. **Corruption = a garbage count in a regexp-tree node.** A value-watch (an
   interp RIP-hook at `regexp/syntax.(*parser).checkSize` @ `0x5f95e0`, dumping
   the `*Regexp` and its `Sub` children) showed the root nodes always look sane;
   the huge size comes from a **deep subtree node** whose 16 bytes at offset +16
   (the `Sub` slice header: `len`,`cap`) are clobbered.

2. **Not overlapping allocation.** An interp tracer hooking `runtime.mallocgc`
   (`0x48b520`), `mallocgcSmallNoscan` (`0x424c40`),
   `mallocgcSmallScanNoHeader` (`0x424f40`) and `mcentral.cacheSpan`
   (`0x4288c0`) — logging returned pointers, detecting same-base / overlapping /
   duplicate hand-outs — found **no** double-hand-out in bad runs (idle showed
   only legitimate large-gap reuse). Object-level and span-level both negative.

3. **Use-after-free, confirmed.** Under `GODEBUG=clobberfree=1` (fills freed
   objects with `0xdeadbeefdeadbeef`), the corrupting run's regexp nodes are
   **riddled with `deadbeef`** — e.g. `re=0x…d20 → deadbeef00d4be09
   deadbeefdeadbeef …`, and its `Sub` children likewise. The tree the compile
   goroutine is *actively building and reading* has been **freed by the GC**.
   `checkSize`/`calcSize` then reads `deadbeef` as a size → "expression too
   large". (The non-clobberfree runs show a real reallocated object — often a
   16-byte interface `{*_type in rodata, data on heap}` — sitting in the freed
   slot; that is the *reallocation*, not the cause.)

4. **The reachability discriminator.** Dumping the parser `p` (RAX at checkSize)
   + its `p.stack []*Regexp` backing array under clobberfree caught the decisive
   shape:

   ```
   re=0x24981ed20            → deadbeef (FREED)
   p=0x249783800  p_dead=false                 ← parser ALIVE
   p.stack.ptr=0x24942ab68  backing[0]_dead=false ← backing array ALIVE
   p.stack.backing = [0x24981ed20, …]          ← INTACT pointer to the freed node
   ```

   So the chain `p (alive) → p.stack slice → backing []*Regexp (alive, holding
   valid node pointers)` is fully intact, yet the **nodes those pointers
   reference are freed**. The GC reached the container but the pointed-to nodes
   were not marked → freed while reachable.

**Conclusion:** a **GC reachability / root-scan miss** — the collector considers
a still-referenced regexp node unreachable and frees it; the slot is reused by
another goroutine, corrupting the compile goroutine's live tree.

---

## 4. Ruled out (with the discriminating evidence)

| Hypothesis | Test | Result |
|---|---|---|
| Scalar RAM tearing / atomicity | Make interp `read`/`write` single-copy atomic; A/B under load | 18/40 vs 13/40, p≈0.26 — **no effect** (but see task-165: a *different* real bug) |
| Memory ordering (x86 TSO) | Interp scalar load/store `Acquire`/`Release` | 14/25 vs 45% — **no effect** across Relaxed/AcqRel/none |
| Futex / lock exclusion | pthreads mutex counter (4T×100k) under 3× load | 0/30 lost updates — futex driver excludes correctly |
| Atomic RMW/CAS/xchg lowering | `lock cmpxchg` CAS-increment + `lock xor` binary-ALU, 8 vcpus | pass — genuinely atomic, correct ZF |
| Mis-lifted instruction | caddy-vs-httpserve mnemonic diff (55 caddy-only) | BMI2 (`bzhi`/`shlx`/…) **unlifted → cold** (CPUID-gated); the lifted integer ops are fine; a static mis-lift can't explain the ~45% *race* |
| Async preemption | `GODEBUG=asyncpreemptoff=1` | still corrupts (signals aren't delivered anyway — P3) |
| Allocator double-hand-out | mallocgc / mcache / cacheSpan tracer | no dup/overlap in bad runs (§3.2) |
| Concurrent mark / write barrier | `GODEBUG=gcstoptheworld=2` | still corrupts → **not** a barrier/concurrent-mark race |
| Mark inconsistency | `GODEBUG=gccheckmark=1` | still corrupts, **no** checkmark panic → mark is self-consistent under the (wrong) root set |
| Stack shrinking / `copystack` | `GODEBUG=gcshrinkstackoff=1` | 9/15 — **not** stack-shrink |
| `[]*Regexp` in a noscan span | Guest span-walker: flag backing-array span `noscan=1` at checkSize | 6 BAD runs, **0** flagged → the *current* backing array is scan, not the mechanism (§6) |

GC-dependence is positive evidence: **`GOGC=off` → 0/12** (was 4/12). The bug
requires an actual GC cycle to run.

---

## 5. Tooling built (reusable; exact addresses/offsets)

All are interp scaffolds gated behind an env var (zero cost when unset), plus
the load harness. None committed.

**Guest symbols (stripped `caddy.elf` shares its Go BuildID with the unstripped
`scratch-go/caddy`, so vaddrs match):**

| Symbol | Addr |
|---|---|
| `regexp/syntax.(*parser).checkSize` | `0x5f95e0` |
| `regexp/syntax.(*parser).calcSize` | `0x5f9780` |
| `runtime.mallocgc` | `0x48b520` |
| `runtime.mallocgcSmallNoscan` | `0x424c40` |
| `runtime.mallocgcSmallScanNoHeader` | `0x424f40` |
| `runtime.(*mcache).nextFree` | `0x424760` |
| `runtime.(*mcache).refill` | `0x428180` |
| `runtime.(*mcentral).cacheSpan` | `0x4288c0` |

**Go 1.26 linux/amd64 runtime offsets (via `gdb` DWARF on the unstripped
binary — `gdb.lookup_type("runtime.mspan")['spanclass'].bitpos//8`):**

| Field | Offset |
|---|---|
| `runtime.mheap_` (global) | vaddr `0x38335e0` |
| `mheap.arenas` | `+66008` |
| `heapArena.spans` | `+0` (a `[8192]*mspan`) |
| `mspan.startAddr` | `+24` |
| `mspan.npages` | `+32` |
| `mspan.spanclass` | `+98` (`uint8`; **low bit = noscan**) |

**Span walker** (addr → covering `mspan`, linux/amd64):
```
arenaBaseOffset = 0xffff800000000000        // amd64
ai   = (addr + arenaBaseOffset) >> 26        // heapArenaBytes = 64 MiB
l2   = ai & ((1<<22) - 1)                     // arenaL1Bits = 0
l1arr = *(mheap_ + 66008)                     // arenas[0]  -> *[1<<22]*heapArena
ha    = *(l1arr + l2*8)                        // *heapArena
span  = *(ha + 0 + ((addr>>13) & 0x1fff)*8)    // heapArena.spans[pageIdx]
spanclass = *(span + 98)                        // low bit = noscan
```
Verified working: healthy `[]*Regexp` backing arrays are always in **scan** spans
(spanclass even), nodes likewise.

**ABI notes:** Go internal register ABI — receiver/args in `RAX, RBX, RCX, RDI,
RSI, R8, R9, …`; results in `RAX…`. At a function's first instruction
`[RSP]` = return address (used to hook a call's return: record retaddr at entry,
match `block.guest_start == retaddr` on the way back).

---

## 6. Why the span-noscan probe came back negative (the timing confound)

`p.stack` **grows** during parsing (`append` reallocates the backing array). The
array observed at `checkSize` (call it `v2`, correctly in a scan span) is
**newer** than the array that was live at the GC which freed the nodes (`v1`).
`v1`'s pointers were copied into `v2`, so `v2` legitimately holds pointers to
already-freed nodes. **`checkSize` is downstream of the free** — the span state
there does not reflect the freeing GC. Any check anchored at `checkSize` sees a
consistent post-hoc snapshot, not the fault.

---

## 7. Refined hypothesis + next steps

**Most likely:** a GC **root-scan miss of a transient node pointer**. A freshly
created `*Regexp` is referenced only by a **register / stack local** in the
compile goroutine during a GC that runs mid-parse; our engine stops that
goroutine for the scan in a state where the pointer is not presented to Go's
stack-map-driven scan → node freed → then linked into `p.stack` dangling → later
read as `deadbeef`. Consistent with every observation: `p` + backing stay alive
(their slots *are* scanned; it's the in-flight pointer that's missed),
`gccheckmark`-clean (self-consistent under the wrong root set), STW-fails (a
root-set miss, not a barrier issue), needs concurrency (GC must overlap the
parse). Async preemption is excluded, so suspect **cooperative-preempt /
safepoint `g.sched` (sp/pc/bp)** or the register/stack-map presentation at the
scan point.

**Next (heavier, dedicated — instrument at GC time, not at checkSize):**
1. Hook the free path (`runtime.(*mspan).sweep` / `freeSpan` / the clobberfree
   writer) to log freed address + PC, and cross-reference with the compile
   goroutine holding it.
2. Hook `runtime.scanstack` / `scanframe` for goroutine 1 and dump the roots it
   collects vs the live tree.
3. Hook the preempt/safepoint save (`gopreempt`/`morestack`/`systemstack`) and
   validate goroutine 1's `g.sched` sp/pc/bp at the scan point.

---

## 8. Spin-off fix (committed): task-165

While excluding the memory-primitive hypotheses, a **separate, real**
MT-correctness bug was found and fixed (commit `0727605`):

`Memory::read`/`write` went through `as_mut_slice()`/`as_slice()` — a
`&mut [u8]`/`&[u8]` over the *whole* shared backing while other vcpus hold atomic
references into it (mutable-aliasing UB). The optimizer could reorder/elide a
plain guest store against a concurrent atomic RMW/CAS on the same location,
breaking mutual exclusion (a cmpxchg-acquire + plain-store-release spinlock lost
updates at 8 vcpus). Fixed by routing scalar read/write through a raw backing
pointer + `AtomicU{8,16,32,64}` `Relaxed` (single-copy atomic; zero cost on
x86 — a `Relaxed` atomic load/store lowers to the same `mov`). Regression test:
`x86jit-tests/tests/mt_atomic_store_coherence.rs`. This does **not** fix
task-161 (Go's lock release uses atomic `xchg`, dodging that exact
manifestation), but it is a genuine correctness win with a deterministic test.

---

## 9. One-line summary

caddy corruption = **Go GC frees a live regexp-tree node** (use-after-free) via a
**reachability/root-scan miss**, contention-gated and interp-exposed; confirmed
by `clobberfree` deadbeef in live nodes + the alive-container/dead-node
discriminator; **not** alloc double-hand-out, current-container-noscan,
stack-shrink, write-barrier, memory-primitive, or a static mis-lift. Repro:
`3 × nproc` oversubscription, fresh processes.

> **Superseded in part by Session 2 (§10).** The "GC root-scan **race**" framing
> below is wrong: the corruption is **not** GC-timing-dependent (fully-STW GC does
> not fix it) and **not** concurrency-dependent (GOMAXPROCS=1 does not fix it). The
> free-of-a-live-node is real, but it is *downstream* — a pointer is corrupted
> deterministically first; GC then frees the now-unreachable node. Read §10.

---

## 10. Session 2 (2026-07-08) — reframe: not a GC race; a regexp-path miscompute

### 10.1 caddy on the JIT (nobody had run it) → far more sensitive

Running `caddy version` through the **JIT** backend fails **~70% of runs at
baseline with zero external load** (interp is 12/12 clean without load). Symptoms
vary run-to-run: `regexp: expression too large`, `found bad pointer in Go heap`,
or a wild `UnmappedMemory` write — i.e. broad, nondeterministic corruption a
downstream GC/deref trips on.

### 10.2 High-power discriminators (all on the JIT baseline, no external load)

| knob | result | conclusion |
|---|---|---|
| `asyncpreemptoff=1` | 13/16 BAD (no change) | dropped-SIGURG / async-preempt **OUT** |
| `gcstoptheworld=2` | 16/16 BAD (no help) | concurrent-mark / write-barrier **OUT** |
| `GOMAXPROCS=1 / 2 / unset` | flat ~60–80% | mutator-parallelism **OUT** |

Same three knobs on the **interp** minimal repro (§10.3): `gcstoptheworld=2`,
`gcstoptheworld=2,asyncpreemptoff=1`, and `GOMAXPROCS=1` **all stay 6/6 BAD**.
→ The corruption is **independent of GC mode and of thread parallelism**. It is
not a GC-scan-vs-mutator race. GC is only the tripwire.

Also noted (real but *not* this bug): the JIT lowers ordinary guest loads/stores
with plain `MemFlags::trusted()` — it never got the task-165 single-copy-atomic
treatment. Benign on an x86 host (aligned ≤8 B `mov` is atomic), a latent gap on
weaker hosts.

### 10.3 Minimal repro — `rgx.elf` (2.7 MB, no caddy, no external load)

A ~30-line Go program reproduces on **interp in ~2 s**, `GOMAXPROCS=1`, no load:

```go
package main
import ("fmt"; "regexp"; "runtime")
func main() {
    pats := []string{`^(a|b)*c[0-9]+$`, `(?P<y>\d{4})-(?P<m>\d{2})-(?P<d>\d{2})`,
        `\b\w+@\w+\.\w+\b`, `([A-Za-z]+)\s+(\d+(\.\d+)?)`, `(https?)://([^/\s]+)(/[^\s]*)?`}
    n := 0
    for i := 0; i < 600; i++ {
        for _, p := range pats { n += regexp.MustCompile(p).NumSubexp() }
        if i%256 == 0 { runtime.GC() }
    }
    fmt.Println("done", n)
}
```
Build: `CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build`. Run via the probe
(§10.5). **Consistent fault signature:** `UnmappedMemory { addr: 0|2, access:
Read } at rip=0x4b0f8e` = `regexp/syntax.(*Regexp).MaxCap`'s `cmpb $0xd,(%rax)`
with a **nil/garbage receiver** — a `*Regexp` that got nilled/corrupted upstream.

### 10.4 It is **regexp-path-specific** — three clean controls

Same GC-churn harness, no regexp — all **clean** (interp, `GOMAXPROCS=1`):

- `tree.elf` — recursive pointer-tree build + `runtime.GC()` → 6/6 OK
- `copy.elf` — large & overlapping `[]*int` `copy()` (forces `rep movsq`) → 5/5 OK
- `deep.elf` — 1500-deep recursion (forces `morestack`/`copystack`) → OK

→ rules out **general GC/alloc**, **`rep movsq`/memmove**, and **stack-growth**.
The trigger is something `regexp/syntax` (parse + `syntax.Compile`) does that
these do not.

### 10.5 Instruction diff bottomed out → shared-lifter / operand-form suspicion

Logging distinct executed instructions (env-gated `note()` in `lift.rs`, reverted):

- **Mnemonic** granularity: `rgx − (tree ∪ copy ∪ deep ∪ httpserve)` = **∅**.
- **iced `Code`** granularity (operand form): diff = `{Seto_rm8, Shr_rm8_imm8}` —
  both single-byte writes through the generic rm8 path the clean programs also
  use → coincidental, **not** the corruptor.

Both backends corrupt **identically** → the fault is in the **shared lifter**
(IR), not backend codegen. Since no unique instruction *form* isolates it, the
corruptor is a **common instruction in a regexp-specific data/control pattern**,
or a **block-formation** issue in the lifter that manifests on regexp's code
shape. Bisect: the bug is present at `eaaf0db` (predates the doc-30 guard-page /
SIGSEGV-resumable work and task-165/163 — those are excluded). Note the probe
uses `hostmem::reserve` (`protect: None`), so **no guard pages / no in-span
faults** are involved in the repro; `reserve()` maps the whole span RW, so a
wrong effective-address **silently** hits the wrong span offset (no trap).

### 10.6 Tooling (uncommitted, working tree)

- `x86jit-tests/tests/caddy_probe.rs` — generic ELF probe. `BACKEND=interp|jit`,
  `X86JIT_ELF=<path>` (else embedded caddy), `X86JIT_ARGV0`, `GUEST_GODEBUG`,
  `GUEST_GOMAXPROCS`. Prints one `PROBE:` line (OK/BADPTR/REGEXP/GAP/OTHER).
  Cannot be committed as-is (`include_bytes!` of gitignored `caddy.elf`).
- `Guest::build_parts` (test support) — exposes the built triple for the probe.
- Go repro sources: `rgx.go` / `tree.go` / `copy.go` / `deep.go` (recipe above).

### 10.7 Traced mechanism — allocator double-hand-out (value-watch)

Minimized to a **single pattern**: `\b\w+@\w+\.\w+\b` (`PAT=2`) corrupts 3/3; the
other four patterns are clean. Then value-watched the corrupted slot end-to-end
(env-gated `LOADWATCH`/`STOREWATCH` hooks in `interp.rs` `Load`/`Store`/`VStore`/
`string_run` + `memory.rs` `write`/`write_bytes`/`atomic_rmw`; all reverted).
Heap layout is **deterministic** under `GOMAXPROCS=1` (slot address stable across
runs), which makes the watch possible.

Chain, at a stable heap slot (e.g. `0x2494780b0`, backing start `0x249478080`):

1. Fault `UnmappedMemory addr=0 @rip` (varies: `MaxCap`, `(*parser).collapse`, …)
   = a `[]*Regexp` slot read as **nil**.
2. The last *scalar* store to that slot wrote a **valid** `*Regexp`; then a
   **non-scalar** write (SSE `movdqu`, size 16, `val=0`) zeroed it.
3. That write is `runtime.memclrNoHeapPointers` (`0x481791`/`0x4817a3`), clearing
   `[rdi=0x249478080, +0x40)`.
4. Its caller (`ret=0x41e065`) is **`runtime.mallocgcSmallScanNoHeader`** →
   `mcache.nextFree`: the allocator is **zeroing a freshly handed-out object**.

So `mallocgc` **hands out memory that still holds the live `[]*Regexp` slice**,
and the zero-on-alloc nils a live pointer slot. Watching the backing start over
the whole run shows a **cyclic reuse** (`build slice → mallocgc re-alloc+memclr →
new object`) every iteration — normal *when the slice is already dead*. The fatal
iteration hands the memory out **while the slice is still live** (still read by
`collapse`/`MaxCap`).

Root: GC **deterministically reclaims the live `[]*Regexp`** (a reachability /
scan miss), sweeps it onto the free list, and `mallocgc` re-serves it. It is a
*miss*, not a *race*: `gcstoptheworld=2` (fully-STW mark) does not fix it and it
is `GOMAXPROCS`-independent — so the mark/scan is deterministically wrong, not
mistimed. Regexp-specific because only that object graph / stack layout hits the
missed-reference condition (`\b`+`\w` in `PAT=2`).

### 10.8 Next

1. **Pin the scan miss.** Instrument the GC scan at the point it *frees* the
   live backing: hook `runtime.greyobject` / `scanobject` / `scanstack` (or the
   sweep free-list link) for the span containing `0x249478080` and dump what
   reference to it was **not** followed — a stack slot with the wrong map, a
   register (async-preempt register map — SIGURG is dropped, §10.2), or a heap
   word the pointer bitmap calls scalar.
2. Check the **stack-map vs safepoint PC**: if the parser holds the slice only in
   a register / a slot live at PC *A* but GC stops it at PC *B*, the map mismatch
   drops it. Compare `g.sched.pc` at the scan against the parser's real PC.
3. **Unicorn lockstep** (repo has the oracle) as ground truth if the scan-miss
   mechanism stays elusive: first x86jit-vs-unicorn state divergence = the
   mislowered instruction that produced the missed/blurred reference.
