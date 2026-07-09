# x86jit-cli

Run an **unmodified host x86-64 Linux binary** under the x86jit recompiler —
**no recompilation**.

Point it at an ELF on your system; its shared libraries are served straight from
the host rootfs (`/` by default), so a normal dynamic binary (`/usr/bin/echo`,
coreutils, and larger payloads) runs as-is under the interpreter or the JIT.

It's thin glue over [`x86jit-run`](../x86jit-run/): the OCI runner already loads
dynamic ELFs and resolves their interpreter + `DT_NEEDED` libs inside a rootfs —
a host binary is just that with `rootfs = /`.

## Usage

```
x86jit-cli [OPTIONS] <BINARY> [GUEST_ARGS]...
```

```sh
x86jit-cli /usr/bin/echo hello world
x86jit-cli -b interp ls -la /tmp
echo hi | x86jit-cli /usr/bin/cat
```

Key options:

- `-b, --backend <interp|jit>` — engine (default: `jit`).
- `--cpu <baseline|v2|v3|v4|default>` — the guest CPU feature level CPUID
  advertises. `v4` advertises AVX-512; a guest then traps on any AVX-512 op the
  lifter can't yet execute.
- `-r, --rootfs <DIR>` — the filesystem the guest sees (default: `/`).
- `-L, --lib <DIR>`, `-e, --env <K=V>`, `--no-inherit-env` — library search
  path and environment control.

> **Note:** file syscalls hit **real host files** under the rootfs — a writing
> guest writes to your disk. Point `--rootfs` at a throwaway tree if that
> matters.

## License

MIT OR Apache-2.0.
