# x86jit-run

Runs an **OCI/Docker image on the x86jit recompiler** — the glue crate.

It joins the pieces:

- [`x86jit-oci`](../x86jit-oci/) turns a `docker save` tarball into a rootfs +
  run config,
- [`x86jit-elf`](../x86jit-elf/) loads the entrypoint (static / PIE / dynamic,
  resolving its interpreter and `DT_NEEDED` libs inside the rootfs),
- [`x86jit-linux`](../x86jit-linux/) services syscalls,
- and [`x86jit-core`](../x86jit-core/) executes it.

This crate is **glue only** — no new engine or OS logic.

## Library

The `run_*` family drives a run at increasing levels of control — from a whole
image (`run_image`) down to explicit argv / stdin / `GuestCpuFeatures` / options
(`run_config_argv_opts`). Both the interpreter and JIT engines are selectable
(`EngineKind`), so a run can be executed on each and compared.

## Binary

```sh
cargo run -p x86jit-run -- <image.tar> [--backend interp|jit|both]
```

`--backend both` runs the image on each engine and flags any divergence — the
differential invariant, end to end on a real payload.

See [`spec.md`](../backlog/docs/design/spec.md) for the design; the OCI track is
covered in the backlog docs.

## License

MIT OR Apache-2.0.
