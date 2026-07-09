# x86jit-oci

An **OCI/Docker image reader** for [x86jit](../).

Parses a `docker save` tarball — the `[{Config, Layers}]` `manifest.json` layout
every `docker save` emits — into a rootfs directory plus the run
[`ImageConfig`] (`Env` / `Cmd` / `Entrypoint` / `WorkingDir`).

No kernel, no container runtime: an image is just layers plus config. Isolation
(namespaces, cgroups) is orthogonal and unneeded to *execute* the payload.

## The boundary guarantee

This crate **deliberately does not depend on `x86jit-core`**. It is pure image
format — the strongest possible statement of the embedder boundary: reading a
Docker image has nothing to do with the recompiler. [`x86jit-run`](../x86jit-run/)
is the glue that joins this reader to the engine and the
[Linux embedder](../x86jit-linux/).

```rust
use std::path::Path;
use x86jit_oci::load_image;

// Extract the image's layers into `rootfs`, returning its run config.
let config = load_image(Path::new("app.tar"), &rootfs)?;
let argv = config.argv();   // Entrypoint + Cmd; config.env / .working_dir drive the run
```

See [`spec.md`](../backlog/docs/design/spec.md) §1 for the boundary rule.

## License

MIT OR Apache-2.0.
