# OCI test fixtures

`docker save` tarballs the OCI tests load (`x86jit-oci/tests/`, `x86jit-run/tests/`).

Small, pinned images are **committed** so the suite is self-contained:

| Fixture | Image | Class |
|---------|-------|-------|
| `hello-world.tar` | `hello-world` | static ET_EXEC |
| `busybox-musl.tar` | `busybox:musl` | static-PIE (musl) |
| `busybox-glibc.tar` | `busybox:glibc` | dynamic (glibc) |
| `alpine.tar` | `alpine` | dynamic (musl) |

## Large / moving-target images (pulled, not committed)

Large or drifting images aren't committed as tars — they're **pulled from the
registry**, digest-pinned, via the shared `pull_image` helper (decision-10). No
`docker save`, no committed blob: `skopeo copy … docker-archive:` writes a tar
`load_image` already reads, cached under `target/oci-pull-cache/<digest>.tar`.

`x86jit-run/tests/ubuntu.rs` pulls ubuntu (`dash -c 'echo …'` three ways,
interp == JIT — the modern-glibc dynamic-loader path plus the SSSE3 string
routines glibc selects once SSE4.1/4.2 are masked, decision-2) and
`registry_pull.rs` pulls busybox. Both are pinned by digest in the test source;
bump the digest to move to a newer release. When `skopeo` is absent or there is
no network egress, they no-op with a note.
