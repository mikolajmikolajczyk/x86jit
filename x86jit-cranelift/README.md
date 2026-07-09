# x86jit-cranelift

Cranelift **JIT backend** for [`x86jit-core`](https://crates.io/crates/x86jit-core).

Inject it in place of the default interpreter to compile hot guest blocks to
native host code. It implements the same `Backend` contract, so guest state is
identical to the interpreter's (the differential invariant, spec §4.1).

```rust
use x86jit_core::{Vm, VmConfig};
use x86jit_cranelift::JitBackend;

let mut vm = Vm::with_backend(VmConfig::flat(0x1_0000), Box::new(JitBackend::new()));
// ... map memory, write code, run — same API as the interpreter.
```

`JitBackend::with_superblocks(caps)` enables region (superblock) formation, and
`JitBackend::with_host_target(HostTarget)` pins the host codegen ISA
(`Native` uses the build host's features; `Baseline` lowers guest AVX to SSE so a
binary built for one host runs on an older one). The guest CPU that CPUID
advertises is a *separate* axis, chosen in the core via `GuestCpuFeatures`.

The `jit` feature is on by default; disable it to build a stub without pulling
Cranelift.

Runnable example: `jit_vs_interp`
(`cargo run -p x86jit-cranelift --example jit_vs_interp`).

## License

MIT OR Apache-2.0.
