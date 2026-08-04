# x86jit-elf

Optional **ELF loader helper** for [`x86jit-core`](https://crates.io/crates/x86jit-core).

Parses an ELF's program headers into the guest memory map and returns the entry
point, plus a `setup_stack` helper that lays out `argv`/`envp`/`auxv` the way a
Linux process expects. A convenience — the core is deliberately format-agnostic;
this crate keeps ELF knowledge out of it.

```rust
use x86jit_elf::{load_static_elf, setup_stack};

let entry = load_static_elf(&mut vm, &image)?;
let sp = setup_stack(&mut vm, stack_top, &[b"prog"], &[])?;
cpu.set_reg(Reg::Rip, entry);
cpu.set_reg(Reg::Rsp, sp);
```

Supports static, static-PIE, and (with `load_dynamic_elf`) dynamically-linked
images. Syscalls trap out as `Exit::Syscall` for the embedder to service — this
crate loads a program, it does not run an operating system. For a Linux userland
(syscall shim, process model, runner) see
[`unemulinux`](https://github.com/unemu-org/unemulinux).

Runnable example: `run_elf`
(`cargo run -p x86jit-elf --example run_elf -- <static-elf>`).

## License

MIT OR Apache-2.0.
