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
