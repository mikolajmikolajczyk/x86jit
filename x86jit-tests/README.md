# x86jit-tests

The **differential test harness** for [x86jit](../) — the reusable spine that
keeps the interpreter, the JIT, Unicorn, and native execution in agreement
(spec §13, [`testing.md`](../backlog/docs/design/testing.md)).

## What's here

- **RON test vectors** ([`vector`]) — self-contained snippets with an initial
  `CpuSnapshot` + memory and per-vector `dont_care_flags` masking.
- **Oracles** ([`oracle`]) — the engine under test (interpreter) versus truth:
  `UnicornOracle` (cross-platform), and native x86-64 execution.
- **The comparator** ([`compare`]) — precise state diff with undefined-flag
  masking.
- **An inline builder** — `Vector::asm(..).init(..).assert_matches_unicorn()`.
- **Fuzzers** — seed-deterministic random-program generation with a
  delta-debugging shrinker; divergences auto-save to `vectors/found/`. (The
  `fuzz_robustness` binary runs >7 min by design — exclude it in the fast loop.)
- **The program corpus** ([`programs/`](programs/)) — a ladder of real binaries
  (busybox, SQLite, Lua, `djpeg`, CPython, Go servers, Caddy) run
  interp-vs-JIT-vs-native.
- **`compat`** — the CI-tested ISA compatibility map.

## Running

```sh
cargo nextest run -E 'not binary(fuzz_robustness)'      # fast suite
cargo nextest run --features unicorn                    # add the Unicorn oracle
cargo run -p x86jit-tests --bin compat -- --write       # regenerate the compat map
```

The `unicorn` feature links the native `libunicorn`; the core harness builds
without it. **Nothing else in the workspace depends on this crate.**

## License

MIT OR Apache-2.0.
