# Test program fixtures

Small, pinned guest ELFs (busybox, djpeg, lua, the `*_go.elf` stand-ins, …) are
**committed** so the suite is self-contained. Each is loaded by a test in
`x86jit-tests/tests/`.

## Large / moving-target fixtures (git-ignored)

`caddy.elf` (~52 MiB static Go) is **not committed** — it is large and a moving
target. Tests that use it are gated on the file being present: they no-op with a
note when it is absent (so a fresh checkout / CI stays green), and run for real
once you build it locally.

Regenerate locally:

```sh
# a static, CGO-free caddy (matches the layout the tests expect)
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
  go build -o x86jit-tests/programs/caddy.elf github.com/caddyserver/caddy/v2/cmd/caddy
```

Then `caddy_serve.rs` (task-153) serves `index.html` over real TCP three ways
(native / interp / tiered JIT).
