# x86jit-cli

Run **x86-64 Linux programs** under the x86jit recompiler — **no recompilation**.
A lib + a single `x86jit-cli` binary with two modes:

- **`run`** (default) — a **host binary**: point it at an ELF on your system; its
  shared libraries are served straight from the host rootfs (`/` by default), so a
  normal dynamic binary (`/usr/bin/echo`, coreutils, …) runs as-is.
- **`oci run`** — pull an **image from a registry** by reference into a temp rootfs
  and run it, docker-run style (built-in OCI-distribution client — no Docker daemon,
  no `skopeo`).
- **`oci load`** — a local **`docker save` image** tarball: extracted to a temp dir
  and run.

This crate folds in what used to be `x86jit-oci` (the image reader, now the `oci`
module) and `x86jit-run` (the runner glue, now the lib's `run_*` API). The library
loads dynamic ELFs and resolves their interpreter + `DT_NEEDED` libs inside a
rootfs; a host binary is just that with `rootfs = /`.

## Usage

```
x86jit-cli [OPTIONS] <BINARY> [GUEST_ARGS]...        # host binary (default)
x86jit-cli oci run [OPTIONS] <REF> [-- CMD...]       # pull from a registry + run
x86jit-cli oci load [OPTIONS] <IMAGE.tar>            # local docker save image
```

```sh
x86jit-cli /usr/bin/echo hello world
x86jit-cli -b interp ls -la /tmp
echo hi | x86jit-cli /usr/bin/cat
x86jit-cli oci run docker.io/library/busybox -- /bin/busybox echo hi
x86jit-cli oci run localhost:5000/app:latest --plain-http --backend both
x86jit-cli oci load image.tar --backend both         # run on each engine, flag divergence
```

`<REF>` is `[registry[:port]/]name[:tag|@digest]` (defaults to Docker Hub, tag
`latest`); `--plain-http` pulls over insecure HTTP for a local `registry:port`.

Host-mode options:

- `-b, --backend <interp|jit>` — engine (default: `jit`).
- `--cpu <baseline|v2|v3|v4|default>` — the guest CPU feature level CPUID
  advertises. `v4` advertises AVX-512; a guest then traps on any AVX-512 op the
  lifter can't yet execute.
- `-r, --rootfs <DIR>` — the filesystem the guest sees (default: `/`).
- `-L, --lib <DIR>`, `-e, --env <K=V>`, `--no-inherit-env` — library search path
  and environment control.

Both `oci run` and `oci load` take `-b, --backend <interp|jit|both>` (`both` runs
each and flags any divergence — the differential invariant, end to end on a real
image).

> **Note:** in host mode, file syscalls hit **real host files** under the rootfs —
> a writing guest writes to your disk. Point `--rootfs` at a throwaway tree if that
> matters.

## Library

The `run_*` family (`run_image`, `run_config_argv_*`, `RunResult`, `EngineKind`)
drives a run at increasing levels of control, and `mod oci` (`load_image`,
`ImageConfig`) reads a `docker save` tarball. `mod oci` has **no dependency on
`x86jit_core`** — reading an image has nothing to do with the recompiler.

## License

MIT OR Apache-2.0.
