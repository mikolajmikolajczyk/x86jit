# AOT / persistent translation cache — execution plan (FD-AOT)

Ready-to-execute plan for persisting compiled JIT code across runs, so a second
run of the same guest skips compilation. Refines the `open-backlog.md` FD-AOT
prereqs with exact code sites, decided after inspecting the backend. Co-authored
with Fable 5 during the 2026-07-06 session; not started (a milestone-sized change
deliberately deferred out of a long session — see the session handoff).

## Header decisions (settled — do not re-litigate)

- **`is_pic` stays `false`.** Relocatability comes from *indirection*, not PIC.
  All cross-unit addresses (helpers, link slots, run-varying constants) go through
  `call_indirect`/table loads, so the compiled buffer contains **no relocations**.
  This supersedes the earlier "`is_pic=true` + self-patch `Abs8` relocs" prereq
  wording. Why: the link slots are *already* baked as plain `iconst` 64-bit
  immediates (`chain_or_link`/`ibtc_or_miss`/`emit_ret_push`, codegen.rs), which
  emit no relocs; the only external references are the 6 helper calls. Making
  those indirect too leaves a reloc-free buffer, and dodges the aarch64
  `Arm64Call` ±128 MiB range hazard that JITModule otherwise hides behind GOT/PLT
  stubs we would have to reimplement.
- **Versions**: cranelift 0.115.x; `memmap2 0.9` is already a workspace dep
  (earmarked for the exec arena, spec §15); add `wasmtime-jit-icache-coherence`
  (already transitively in Cargo.lock) as a direct dep when cranelift-jit is dropped.
- **6 helpers** (not 5): div, string, cpuid, x87, fxstate, crc32.

## B0.1 — reloc-free codegen (under JITModule; committable alone; corpus-gated)

Make compiled functions contain zero relocations while still using JITModule, so
the risky arena swap (B0.2) is a mechanical follow-up with a cheap invariant check.

- `x86jit-cranelift/src/lib.rs` `compile_with` (~288–403): delete the six
  `declare_function(Import)` + `declare_func_in_func` blocks. Instead build each
  helper `Signature` and `builder.import_signature(sig) -> SigRef`; pass
  `(SigRef, helper_fn as u64)` pairs into `codegen::Helpers`.
- `codegen.rs`: change `Helpers` fields from `FuncRef` to `(SigRef, u64)`. Each of
  the 5 call sites (div/string/cpuid/x87/fxstate/crc32 — codegen.rs ~558, 569,
  593, 638, 1327) becomes `let f = self.iconst(addr); builder.ins().call_indirect(sigref, f, &args)`.
- Gate: full differential corpus (busybox/alpine/glibc/sqlite/lua/cpython + native
  oracle, interp==JIT). **Hard stop**: not green after one debug cycle → `git reset`.

## B0.2 — retire JITModule (corpus-gated)

- `compile_with`: build `ctx.func` directly (`UserFuncName::user(0, id)`, signature
  with `isa.default_call_conv()`); `ctx.compile(&*isa, &mut ControlPlane::default())`;
  `let code = ctx.compiled_code().unwrap()`; **assert `code.buffer.relocs().is_empty()`**
  (proves B0.1 held); copy `code.code_buffer()` into the arena.
- Arena: chunked bump allocator over `memmap2::MmapMut` (e.g. 1 MiB chunks, 16-byte
  align). W^X per append: mprotect chunk RW → memcpy → mprotect RX (compile is the
  slow path; no RWX mapping).
- icache: `wasmtime_jit_icache_coherence::clear_cache(ptr,len)` + `pipeline_flush_mt()`
  (required on ARM64, no-op on x86).
- Slots (`Vec<Box<AtomicU64>>`) unchanged. Drop cranelift-jit/-module deps.

## B1 — relocatable units (invisible; no behavior change)

- Per-unit address table: helper addrs + slot addrs (+ any run-varying immediates)
  become loads from `table_base + i*8` instead of baked iconsts.
- Deliver `table_base` via a **new `MemCtx` field** (`x86jit-core/src/jit_abi.rs`) —
  no compiled-signature change; the dispatcher fills it.
- Residual bake to record as a decision: guest RIP constants / return addresses stay
  in code, so a persisted unit is valid only for identical guest load addresses.
  Acceptable v1 (the OCI runner uses fixed EXE_BASE/derived interp base).

## B2 — persist + reload

- Unit on disk: `{ key, code bytes, entry offset, table layout: [Helper(i) | Slot | Const(u64)] }`,
  hand-rolled LE framing (no serde in lib crates).
- **Cache key** = hash(IR stable form) ⊕ crate version ⊕ ISA flags ⊕ consistency
  tier ⊕ mmio window. Hash the IR (FNV-1a-128, not `DefaultHasher` — SipHash keys
  aren't cross-run stable) inside `JitBackend`, so **zero core changes** (materialize
  already receives `IrBlock`). A hit still pays lift, but compile ≫ lift.
- `JitBackend::with_aot_dir(path)`; one file per key under `~/.cache/x86jit/`. Read
  miss / corrupt / version mismatch → silently recompile and rewrite (never panic on
  a bad cache file). Load = map → fill table (fresh slots, current helper addrs) → RX.

## B3 — milestone test (the visible "AOT works")

- `x86jit-tests`: run a guest (e.g. the ubuntu `dash -c 'echo'` path, `tier_up_after`
  low so blocks compile) twice with a tmp cache dir. Second run asserts
  `compiled_fresh == 0` && `aot_hits > 0` (add counters) and byte-identical output,
  plus interp == cache-loaded-JIT. Corruption + version-bump invalidation cases.
- **This test is the definition of done for "make AOT work."**

## Risks

- aarch64 emits different reloc kinds for any residual external ref — the B0.2
  `relocs().is_empty()` assert catches a regression at compile time; verify via the
  manual ARM CI workflow (the reloc leg can't be tested on an x86 host).
- Second run "skips compile" must be asserted via counters, not latency (tier-up
  gating still interprets cold blocks).
