# Coding conventions

Generic conventions that apply regardless of stack. Stack-specific rules live in the **Stack-specific** section at the bottom.

## File naming

- Rust: `snake_case` modules, one primary concept per file. Co-locate tightly related items.
- Keep the module map in [`architecture.md`](architecture.md) in sync when you add a module.

## Imports

- Cross-crate imports use a crate's **public API**, never a `pub(crate)`/private item. That public API has two tiers, both part of the contract:
  - the flattened `lib.rs` re-exports — the embedder-facing surface (`Vm`, `Exit`, `Reg`, …); prefer these;
  - deliberately `pub` modules for tightly-coupled consumers — e.g. `x86jit-core`'s `jit_abi`, `lift`, `interp`, `x87`, `state`, which the `x86jit-cranelift` backend and the test crates import directly (the backend needs the shared `divide`/`string_run`/`exec_x87` helpers and the ABI constants; tests need `lift_block`). These paths are stable API, not internals.

  So: reshuffling *within* a module is safe; moving a `pub` item *between* modules (or narrowing its visibility) is a breaking change.
- Inside a crate, prefer `crate::module::Item` over long relative chains.

## Comments

- **Default: no comment.** Names do the work.
- Add only when the *why* is non-obvious: hidden constraint, subtle invariant, workaround for a specific bug, surprising x86 semantics.
- Never explain *what* the code does — well-named identifiers already do that.
- Don't reference the current task / fix / PR ("added for X", "handles case from #123") — that belongs in the commit message, not the source file.
- **Do** cite the spec.md section that a piece of code implements (e.g. `// §7.1 effective address`) — the spec is the shared map.

## Commits

- Conventional Commits by default (`feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `release:`).
- GPG-signed. The `gpg-uid-guard` pre-commit hook refuses to sign if `user.email` has no matching UID on `user.signingkey`.
- **Never commit without explicit user request.** This rule supersedes any plan acceptance.

## Phase / milestone discipline

- Don't pre-empt later milestones (spec.md §12). If something belongs to M4 (JIT) or M7 (multithreading), don't half-implement it during M1 work.
- The `todo!()` stubs are milestone markers — fill them in milestone order, not opportunistically.
- Don't add error handling, fallbacks, or validation for scenarios that can't happen at the call site. Trust internal code; validate only at system boundaries (the public `map`/`write_bytes`/`run` surface).
- If a refactor would be cleaner alongside a fix but isn't required, defer it — open an issue instead.

## When in doubt

- Read the relevant spec.md section (every module cites one) and the matching ADR in [`../adr/`](../adr/).
- Check GitHub issues for active work.
- Ask the user. Solo project — they're the only deciding authority.

---

## Stack-specific (Rust)

- **Edition:** 2021. **MSRV:** 1.75 (workspace `rust-version`).
- **Lints:** `cargo clippy --all-targets --all-features -- -D warnings` must pass. No warnings in committed code.
- **Error handling:** typed enums per layer (`LiftError`, `MapError`, `MemError`, `MemTrap`, `FaultKind`) — **no `anyhow`/`thiserror` in the core**. The public failure surface is `Exit` (§5.2); lift errors map to `Exit`, they never panic (§7.3).
- **`unsafe`:** confined to the JIT boundary — the compiled-block ABI (§8.2.1) and `CompiledPtr`'s manual `Send + Sync` (§9.1). Every `unsafe` block carries a `// SAFETY:` note. No `unsafe` in the interpreter path.
- **`#[repr(C)]` on `CpuState`** is load-bearing: field offsets are a contract with codegen (§8.2.1). Don't reorder fields without updating the offset logic.
- **x86 semantics traps** (the silent-bug sources, spec.md §16) — encode each once, centrally:
  - 32-bit register writes **zero** the upper 32 bits; 16/8-bit writes preserve them. Encode in the GPR write path, not per-lift (§7.1, §8.2.1).
  - Memory operand ≠ `Val`: it needs effective-address computation + `Load`. Use the operand-lowering helpers (§7.1).
  - Read-modify-write (`add [mem], rax`): compute the effective address **once**, reuse for `Load` and `Store`.
  - RIP-relative is computed against the *next* instruction — use iced's value, don't recompute.
  - FS/GS segment base adds to the address for TLS accesses.
  - Flags use a **`FlagMask`, never a `bool`**: `inc`/`dec` keep CF, logic ops force CF=OF=0, shifts update flags only when count ≠ 0 (runtime-conditional). iced says *which*; you encode *how*.
  - Flags are also **input**: `adc`/`sbb` consume CF; `setcc`/`cmovcc`/`rcl`/`rcr` read flags as data (`GetCond`). Lift can't skip these — 128-bit add chains need `adc`.
  - **Instruction atomicity (pitfall #0):** within one guest instruction emit all trapping ops (load/store) *before* any state commit (WriteReg, flags), or a fault-retry corrupts state. Bake into the lowering helpers (§7 pitfall 3).
- **Aliasing / ownership (the two structural rules that hit early):**
  - Guest RAM is `&Memory` with interior mutability (`UnsafeCell` + manual `Sync`), `write(&self)` — **never `&mut Memory`**. It's shared across vcpus; `&mut` can't model M7 (§8 pitfall). `CpuState` stays `&mut`, per-vcpu.
  - The backend is an **injected `Box<dyn Backend>`** (`Vm::with_backend`), not a config enum — the core can't name the downstream JIT crate (§4.1).
  - JIT inlined memory access needs a bounds+permission check or guard pages — raw `host_base+addr` is host UB (§8.2.3).
- **Extensibility seams (spec.md §17) — keep the discipline, don't build machinery:**
  - Pass the guest mode as a value (`CpuMode`, today only `Long64`); never hardcode the literal `64` at `Decoder::new` or assume "segments are always 0" ad hoc (§17.3).
  - Address computation goes through the single `effective_address` helper — no lift computes an address itself. This is seam 3; it's also just correct for RMW/FS-GS (§17.5).
  - **Do NOT** add `trait ExecutionMode`/`AddressingMode`, mode config, or an API for `Protected32`. Seams are cheap and good code; machinery before a second implementation is forbidden (§17.6). Reject non-64-bit binaries loudly at the loader (§17.7).
- **Test strategy:** differential testing is the primary oracle — run a block natively on an x86 host and compare state; interpreter is the oracle for the JIT (§13). Per-instruction unit tests cover edge cases (overflow, zero, sign). Decoder fuzzing must never panic. Whole-program native-vs-JIT differential is the macro integration oracle (testing.md §12).
