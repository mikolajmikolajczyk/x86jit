# OCI test fixtures

Nearly all OCI image tests now **pull their image digest-pinned from the registry**
with the built-in client (`x86jit-cli/src/registry.rs`) — no `skopeo`, no committed
tar. The refs live as consts in `x86jit-cli/tests/common/mod.rs` (busybox musl/glibc,
alpine, hello-world, ubuntu), pinned by digest; bump a digest to move to a newer
release. Blobs are cached content-addressed under `$X86JIT_OCI_CACHE` (CI persists it
via `actions/cache`), so a registry — public.ecr.aws, no anon rate limit — is hit at
most once per digest. When there's no network egress the tests no-op with a note.

## The one remaining committed tar

| Fixture | Image | Why kept |
|---------|-------|----------|
| `hello-world.tar` | `hello-world` | `oci_load.rs` tests the `docker save` **tar parser** (`load_image`) directly — it needs a local tar by definition. |

`ubuntu.tar` (git-ignored, large) is a leftover from before the registry-pull
conversion and is no longer used.
