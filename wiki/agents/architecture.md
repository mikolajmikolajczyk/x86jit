# Architecture

Workspace shape, data flow, key modules. **Descriptive of the current state**, not aspirational. For *why* the architecture is what it is, see [`../adr/`](../adr/) and [`../design/spec.md`](../design/spec.md).

## Layout

Cargo workspace, four crates (spec.md §2):

```
x86jit/
├── x86jit-core/        # Vm, Vcpu, IR, lift, cache, dispatcher, interpreter — the engine
├── x86jit-cranelift/   # Cranelift JIT backend (feature `jit`, default-on, optional)
├── x86jit-elf/         # ELF64 loader (goblin): load_static_elf + setup_stack (SysV argv/auxv); convenience, NOT core
├── x86jit-tests/       # harness: RON vectors, compare, Unicorn oracle, LinuxShim, corpus, programs/
├── flake.nix           # Nix devShell + package (rust-overlay toolchain)
└── spec.md          # authoritative design spec
```

`x86jit-core` module map (`x86jit-core/src/`):

| Module | Purpose |
|--------|---------|
| `state` | `Reg`, `CpuState` (`#[repr(C)]`, flat GPR file), `Flags` (§3) |
| `memory` | `Memory`, `MemoryModel` (Flat/SoftMmu), `Prot`, `RegionKind`, `MemTrap` (§4) |
| `ir` | `IrOp`, `Val`, `Temp`, `Cond`, `MemOrder`, `IrBlock`, `TempGen` (§6) |
| `lift` | x86 → IR: `lift_block`, operand lowering, `CpuMode` seam, `LiftError` (§7) |
| `interp` | IR interpreter: `interpret_block` over a temps vector, Variant-A flags, RIP-on-trap (§8.1) |
| `disasm` | decode-and-print helper: `disassemble`, `print_disassembly`, `DecodedInsn` (inspection only, §12 M0) |
| `exit` | `Exit`, `AccessKind`, `StepResult`, `FaultKind` (§5, §8) |
| `cache` | `TranslationCache`, `CachedBlock`, `CompiledPtr` (§9) |
| `vm` | `Vm` (shared), `Vcpu` (per-thread), `run()` dispatcher loop (§2, §9.2) |

## Data flow

```
guest bytes → iced-x86 decode → lift → IR (IrBlock) → backend.materialize → CachedBlock
                                                                  │
                       translation cache (guest addr → CachedBlock)
                                                                  │
   dispatcher loop: cache_get(pc) → execute(block) → StepResult ──┴→ Continue (loop) | Exit (return to user)
```

Hot path (RAM access) is **inlined** into generated code — never a callback. Rare events (syscall, MMIO, unknown instruction) **trap out** through `Exit` (§1 boundary rule).

The KVM-style split: **`Vm`** owns shared state (memory + cache); **`Vcpu`** owns per-thread `CpuState` and its own `run()` loop. Many `Vcpu`s over one `Vm` is the path to multithreading (§2, §11).

## Key modules / contracts

- **Backend is not one `execute`.** `materialize(&IrBlock) -> CachedBlock` is backend-dependent; `execute(&CachedBlock)` is uniform and matches on the variant (§8). The interpreter wraps `Arc<IrBlock>`; the JIT compiles to host code.
- **`StepResult`, not `Exit`, from the execution layer** — distinguishes "continue" from "trap out" (§8).
- **Operand lowering (§7.1) sits *beneath* per-mnemonic lift.** Every operand reduces to a `Val`; memory operands expand to effective-address arithmetic + `Load`/`Store`. This is the load-bearing layer — nothing lifts without it.

## Layering rules

- `x86jit-core` depends on `iced-x86` only. No cranelift, no memmap2 in core.
- `x86jit-cranelift` depends on `x86jit-core` + cranelift crates (feature-gated).
- `x86jit-elf` depends on `x86jit-core` only; it is a convenience, the core never parses formats.
- Nothing depends on `x86jit-tests`.
