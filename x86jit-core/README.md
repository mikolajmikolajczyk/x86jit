# x86jit-core

Guest-agnostic **x86-64 → host recompiler** engine, as a pure-Rust library.

Feed it a memory map plus an entry point; it executes guest x86-64 instructions
on any host (x86-64 or ARM64) and returns control through [`Exit`] whenever it
hits something it does not handle itself — a syscall, an MMIO access, or an
instruction the lift does not yet support. File-format parsing, OS syscall
emulation, and devices live in *your* code, not here (see `x86jit-elf`,
`x86jit-linux`).

The core ships the default **interpreter** backend. The optional
[`x86jit-cranelift`](https://crates.io/crates/x86jit-cranelift) crate injects a
**JIT** backend implementing the same contract; the two must agree bit-for-bit
(the interpreter is the JIT's oracle). The guest CPU the emulator advertises via
CPUID/XCR0 is embedder-selectable per run through `GuestCpuFeatures`
(`baseline`/`v2`/`v3`/`v4`), like `qemu -cpu`.

```rust
use x86jit_core::{Exit, Prot, Reg, RegionKind, Vm, VmConfig};

let mut vm = Vm::new(VmConfig::flat(0x1_0000));
vm.map(0, 0x1_0000, Prot::RWX, RegionKind::Ram).unwrap();
vm.write_bytes(0x1000, &[0xB8, 0x05, 0, 0, 0, 0xF4]).unwrap(); // mov eax,5 ; hlt

let mut cpu = vm.new_vcpu();
cpu.set_reg(Reg::Rip, 0x1000);
assert!(matches!(cpu.run(&vm, None), Exit::Hlt));
assert_eq!(cpu.reg(Reg::Rax) as u32, 5);
```

Runnable examples: `raw_bytes`, `mmio_device`
(`cargo run -p x86jit-core --example raw_bytes`).

Design: [`spec.md`](https://github.com/mikolajmikolajczyk/x86jit/blob/main/backlog/docs/design/spec.md).

## License

MIT OR Apache-2.0.
