# x86jit-bench

Per-commit **native vs interpreter vs JIT** timing for [x86jit](../).

Records each workload's runtime under the three execution modes as JSON under
`bench/history/<short-sha>.json`, so results are comparable across commits —
evidence of what each change buys, and where.

## Usage

```sh
cargo run -p x86jit-bench --release -- record [--iters N]   # measure HEAD, store JSON
cargo run -p x86jit-bench --release -- compare <refA> <refB>
cargo run -p x86jit-bench --release -- show <ref>
```

The recorded history backs the CI **perf-regression gate** (a commit that
regresses the JIT vs its baseline is flagged). Pure-refactor commits that trip a
false positive can override with `X86JIT_ALLOW_PERF_REGRESSION=1`.

See [`spec.md`](../backlog/docs/design/spec.md) and the perf-bench notes in the
backlog docs.

## License

MIT OR Apache-2.0.
