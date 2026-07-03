# x86jit — user documentation

Docs for humans embedding `x86jit` as a library. Usage, examples, integration guides. Not for coding agents — agents read [`../agents/`](../agents/).

`x86jit` is a guest-agnostic x86-64 recompiler engine: you give it a memory map plus an entry point, it executes guest instructions and hands control back through `Exit` whenever it hits something it doesn't handle (syscall, MMIO, unknown instruction). File-format parsing (ELF/PE), OS syscall emulation, and devices live in **your** code, not the core (§1).

## Minimal shape

```rust
use x86jit_core::{Vm, VmConfig, MemoryModel, MemConsistency, Reg, Exit, Prot, RegionKind};

// Default backend is the interpreter. For the JIT:
//   Vm::with_backend(cfg, Box::new(x86jit_cranelift::JitBackend::new(..)))
let mut vm = Vm::new(VmConfig {
    memory_model: MemoryModel::Flat { size: 64 << 20 },
    // Consistency tier (matters on ARM hosts only): Fast → AcqRel → FullTso.
    // Escalate per workload if a multithreaded guest misbehaves (spec §8.2.3).
    consistency: MemConsistency::Fast,
});

// map + load guest bytes (an ELF loader would do this for you — see x86jit-elf)
vm.map(0x1000, 0x1000, Prot::RX, RegionKind::Ram)?;
vm.write_bytes(0x1000, &code)?;

let mut cpu = vm.new_vcpu();
cpu.set_reg(Reg::Rip, 0x1000);

loop {
    match cpu.run(&vm, Some(100_000)) {
        Exit::Syscall => { /* read args via cpu.reg(), set result, continue */ }
        Exit::Hlt => break,
        other => { /* handle mmio / unknown / budget */ break }
    }
}
```

<TBD: expand with a runnable end-to-end example once M2 (static ELF hello-world) lands.>
