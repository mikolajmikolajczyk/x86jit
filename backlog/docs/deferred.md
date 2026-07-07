---
id: doc-5
title: 'Deferred'
type: guide
created_date: '2026-07-06 11:25'
---

# Deferred

Things **deliberately not implemented yet**. If something seems missing and is listed here, don't add it unprompted — there's a milestone reason (spec.md §12, §14). Each entry: what, why deferred, when to revisit.

## Format

```markdown
### <Feature / behavior>

- **Why deferred:** <one paragraph>
- **Revisit when:** <trigger condition / milestone>
- **Tracked in:** <issue #, if any>
```

## Entries

### fork / execve from a threaded process (go-caddy P2)

- **Why deferred:** once a process has spawned a thread (`clone(CLONE_VM)`), neither Linux fork semantics (duplicate only the calling thread) nor execve (kill all siblings, replace the image) is modeled. The threaded driver (`x86jit-linux/src/thread.rs`) handles them without ever panicking the host: **fork/vfork/clone-without-CLONE_VM return the guest a real `-EAGAIN`** (fork's resource-exhaustion errno, which every runtime handles), and **execve/wait4/blocking-pipe-read return a fatal typed `ProcError`** naming the op (faking an execve errno would silently corrupt a run). A single-threaded process is unaffected — it still forks/execs through the deferred scheduler (`x86jit-linux/src/proc.rs`).
- **Revisit when:** a real threaded guest needs `posix_spawn`/`system()`/`exec` — then model fork-of-calling-thread and execve-kills-siblings properly.
- **Tracked in:** go-caddy P2.8 (task-109.9).

### JIT backend (Cranelift codegen)

- **Why deferred:** interpreter must exist and be correct first — it is the oracle for the JIT (§13). Building both at once removes the reference to validate against.
- **Revisit when:** M4, after the interpreter runs the M2 corpus. Build incrementally: empty "return Continue with new RIP" block first, then `IrOp` by `IrOp` (§8.2.3).
- **Tracked in:** —

### Lazy flags (Variant B)

- **Why deferred:** materialized flags (Variant A, §3.2) are simpler and correct. Lazy flags are a performance optimization that complicates the IR.
- **Revisit when:** M5, once the JIT works and profiling shows flag computation is hot.
- **Tracked in:** —

### SoftMmu memory model

- **Why deferred:** `Flat` (one contiguous host buffer) is fastest and enough while the guest space is dense (§4.1).
- **Revisit when:** the guest uses sparse / high addresses (e.g. near the top of the 64-bit space) that `Flat` can't back.
- **Tracked in:** —

### SMC (self-modifying-code) invalidation

- **Why deferred:** requires per-page "has translated code" tracking and cache invalidation on write (§10). Nothing needs it until a guest modifies its own code.
- **Revisit when:** M6, or the first time a real program/game rewrites its own `.text`.
- **Tracked in:** —

### Multithreading + TSO barriers

- **Why deferred:** first version is single-threaded. The `Vm`/`Vcpu` split and `CompiledPtr: Send + Sync` are in place so this doesn't require a rewrite (§9.1, §11).
- **Revisit when:** M7. Needs cache synchronization + `MemConsistency` tiers (`Fast`/`AcqRel`/`FullTso`) in codegen (§8.2.3).
- **Tracked in:** —

### SIMD (SSE/AVX)

- **Why deferred:** big, self-contained chapter. XMM/YMM state and vector-instruction lift (§3.1, §12 M8+). Real games need it, but nothing on the critical path does.
- **Revisit when:** M8+.
- **Tracked in:** —

### Block chaining / superblocks / traces

- **Why deferred:** performance optimization that stitches blocks without returning to the dispatcher (§12 M5).
- **Revisit when:** M5, after correctness is locked and profiling justifies it.
- **Tracked in:** —

### Background tier-up — deliberate exclusions (bg-tier, doc-27)

Background tier-up shipped (a single compiler thread per `JitBackend`; opt-in via
`Vm::set_tier_up_background`). Three parts were left out on purpose:

- **Compiler-thread pool (one worker only).** The worker holds the same `Mutex<Jit>`
  the foreground `materialize` uses, so N workers can't compile in parallel until the
  `JITModule` is retired. **Revisit when:** FD-AOT B0.2 removes the shared module (§9.1).
  **Tracked in:** doc-27 D3.
- **Background *region* (superblock) tier-up.** Only single blocks tier up in the
  background today; hotness-gated region formation off the vcpu is a separate rung.
  **Revisit when:** BGT-6. **Tracked in:** task-140.
- **Per-span epoch (global epoch today).** A single invalidation epoch means an
  unrelated SMC/map can reject an in-flight compile that then self-heals by
  resubmitting — correct, but wasteful under heavy code-page churn. A per-span epoch
  would scope rejections. **Revisit when:** if the `tier_bg_rejected` counter shows it
  matters. **Tracked in:** doc-27 (risks).

### Threaded virtual clock (VCLK, doc-28 / decision-6) — deliberate exclusions

The mt-mode virtual monotonic clock shipped (rate-controlled value, real host
blocking, idle-only wait credits). Four parts were left out on purpose (doc-28 M6):

- **No host-time governor ("max virtual speedup" rate-limit).** Nothing asserts
  wall-time pacing, so virtual time is not capped to real elapsed. **Revisit when:**
  a guest legitimately needs wall-clock-correlated time (rate limiters, TLS validity)
  — that reopens the governor alternative (decision-6 trigger).
- **One clock domain.** `CLOCK_REALTIME` == `CLOCK_MONOTONIC + CLOCK_BASE_SEC`; no
  per-clock drift, no `CLOCK_THREAD_CPUTIME_ID`. **Revisit when:** a guest needs a
  distinct clock's semantics.
- **Blocking host-fd I/O consumes no virtual time.** A real host wait on a real fd
  (a blocking socket `read`/accept) is invisible to the guest clock — bounded risk
  R3. **Revisit when:** a real workload misbehaves timing a host-fd operation (then
  credit blocking fd I/O too).
- **`SYS_POLL` stays instant-ready and time-free.** Go blocks in epoll/futex, not
  poll. **Revisit when:** a real guest needs poll timeouts.

Also **not built**: an integration clock-*discriminator* test. A `ReadHeaderTimeout`
`go_http` variant was prototyped and dropped — its accept→read window is too short in
guest-progress terms to distinguish the idle-CAS credit from the (wrong) `fetch_max`
one (both pass ≥500 ms), and the deadline-free eager leg passes under either credit
rule and even under the host-anchored clock (the eager empty-response was the fixture
bug, not the clock). The CAS gate's speed-invariance is pinned by a unit test
(`busy_process_expiry_does_not_credit`) instead; a real long-span-deadline workload
would be the honest integration gate. **Revisit when:** such a workload enters the corpus.

### Optional hook-based API (alongside return-based)

- **Why deferred:** the core is return-based (`run()` → `Exit`) on purpose (§5.1). Hooks are a possible debugging convenience, not a contract.
- **Revisit when:** after M4, only if hooks prove useful. The return-based core stays authoritative (§14).
- **Tracked in:** —

### Other x86 processor modes (32-bit protected / real) + multi-mode machinery

- **Why deferred:** the library is **x86-64 long mode only** (§1). §17 leaves three cheap *seams* (mode as a value not the literal `64`, mode in the cache key, single `effective_address` choke-point) so a mode could be added later — but **building the machinery now is forbidden**: no `trait ExecutionMode`/`AddressingMode` with one impl, no parametrizing things identical in 32/64-bit, no API for `Protected32` nobody wrote. Empty abstractions never validated by a second implementation come out wrong.
- **Revisit when:** a real second mode is actually needed — then design the abstraction with the concrete case in hand. Today: reject non-64-bit binaries loudly at the loader (§17.7).
- **Tracked in:** —

### Other guest architectures (ARM/MIPS/6502) as a second front-end

- **Why deferred:** the IR already supports this — a new decoder + lift targeting the same IR reuses every backend (§17.1). Not a seam question, just unwritten work; don't scaffold for it speculatively.
- **Revisit when:** there's a concrete second-arch need.
- **Tracked in:** —
