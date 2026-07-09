# x86jit-linux

The **Linux x86-64 userland embedder** for [x86jit](../) (spec §1/§4.1).

[`x86jit-core`](../x86jit-core/) executes guest instructions and traps out on
`Exit::Syscall`. This crate is the *embedder* that services those traps: the
Linux syscall shim ([`shim::LinuxShim`]), the guest filesystem, and — as the OCI
track climbs — the multi-process model (fork / exec / wait / pipe).

None of this belongs in the core. File formats, OS syscalls, and devices live
here, on the embedder side of the boundary — the whole point of the
guest-agnostic design.

## Scope

- Syscall shim: file I/O, memory management (`mmap`/`mprotect`), process and
  thread primitives, futex, signals — extended on demand as real programs
  require them.
- A guest filesystem rooted at an embedder-chosen rootfs (so a guest's file
  syscalls hit real host files under that root).
- A process scheduler for multi-process workloads (shell pipelines out of a
  Docker image).

## Where it sits

```
guest ─(Exit::Syscall)→ x86jit-linux (shim + fs + process model) ─→ host kernel
```

Used by [`x86jit-run`](../x86jit-run/) (OCI images) and
[`x86jit-cli`](../x86jit-cli/) (host binaries). See
[`spec.md`](../backlog/docs/design/spec.md) §1/§4.1 for the boundary rule.

## License

MIT OR Apache-2.0.
