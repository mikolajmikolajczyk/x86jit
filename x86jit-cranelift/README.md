# x86jit-cranelift

Cranelift **JIT backend** for [`x86jit-core`](https://crates.io/crates/x86jit-core).

Inject it in place of the default interpreter to compile hot guest blocks to
native host code. It implements the same `Backend` contract, so guest state is
identical to the interpreter's (the differential invariant, spec §4.1).

```rust
use x86jit_core::{MemoryModel, MemConsistency, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

let mut vm = Vm::with_backend(
    VmConfig { memory_model: MemoryModel::Flat { size: 0x1_0000 }, consistency: MemConsistency::Fast },
    Box::new(JitBackend::new()),
);
// ... map memory, write code, run — same API as the interpreter.
```

`JitBackend::with_superblocks(caps)` enables region (superblock) formation.
The `jit` feature is on by default; disable it to build a stub without pulling
Cranelift.

Runnable example: `jit_vs_interp`
(`cargo run -p x86jit-cranelift --example jit_vs_interp`).

## License

MIT OR Apache-2.0.
