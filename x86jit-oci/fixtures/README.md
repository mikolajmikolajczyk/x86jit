# OCI test fixtures

`docker save` tarballs the OCI tests load (`x86jit-oci/tests/`, `x86jit-run/tests/`).

Small, pinned images are **committed** so the suite is self-contained:

| Fixture | Image | Class |
|---------|-------|-------|
| `hello-world.tar` | `hello-world` | static ET_EXEC |
| `busybox-musl.tar` | `busybox:musl` | static-PIE (musl) |
| `busybox-glibc.tar` | `busybox:glibc` | dynamic (glibc) |
| `alpine.tar` | `alpine` | dynamic (musl) |

## Large / moving-target fixtures (git-ignored)

`ubuntu*.tar` is **not committed** — a full ubuntu image is ~40 MB and
`ubuntu:latest` drifts release to release. Tests that use it are gated on the
file being present (they no-op with a note when it is absent). Regenerate locally:

```sh
docker pull ubuntu:latest
docker save ubuntu:latest -o x86jit-oci/fixtures/ubuntu.tar
```

The ubuntu test (`x86jit-run/tests/ubuntu.rs`) runs `dash -c 'echo …'` three ways
(interp == JIT), exercising the modern-glibc dynamic-loader path plus the SSSE3
string routines (`pshufb`/`palignr`) glibc selects once SSE4.1/4.2 are masked
(see `wiki/decisions/2026-07-06-cpuid-drop-sse4.md`).
