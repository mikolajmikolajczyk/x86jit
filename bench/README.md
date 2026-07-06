# Benchmarks — native vs interpreter vs JIT, per commit

Evidence of what each change buys and where. `x86jit-bench` runs a fixed set of
workloads three ways (native subprocess, interpreter, JIT), and stores the min-of-N
timings as JSON under [`history/`](history/) keyed by the commit's short SHA, so
results can be compared across commits.

## Run

Always `--release` (debug timings are meaningless):

```sh
cargo run -p x86jit-bench --release -- record            # measure HEAD, write history/<sha>.json
cargo run -p x86jit-bench --release -- record --iters 5  # more samples (min-of-N)
cargo run -p x86jit-bench --release -- compare <A> <B>   # delta table between two commits
cargo run -p x86jit-bench --release -- show <ref>        # print one record
cargo run -p x86jit-bench --release -- list              # all stored records
```

`<A>`/`<B>`/`<ref>` accept a short SHA, `HEAD`, or a branch name (resolved via
`git rev-parse --short`).

## Workflow (per commit)

1. Commit your change (a clean tree — `record` refuses to attribute dirty-tree
   timings to a commit; it warns and marks `dirty: true`).
2. `record` — writes `history/<sha>.json`.
3. Commit that JSON. Because the record is taken *after* the code commit, it lands
   in a follow-up commit; that's the inherent one-commit lag.

To backfill an old commit: `git checkout <sha>`, `record`, `git checkout -` and
commit the JSON on your branch.

## Reading the numbers

- **Timings only compare on the same machine.** Each record tags `host` + `cpu`;
  `compare` warns if they differ.
- **JIT timing is end-to-end and cold** — load + compile + run, a fresh `Vm` each
  iteration. That's the honest one-shot cost (Cranelift compile included), which is
  why compile-bound one-shots can be *slower* than the interpreter.
- **Counters** (from the JIT run) are evidence of what fires: `chained` (link-slot
  / call / return / IBTC transfers that skipped dispatch), `ibtc_filled`,
  `fast_hits` (fast-resolve cache), `misses` (blocks lifted — high on one-shots =
  the compile-bound signal).

## Workloads (two ends of the spectrum)

| name | kind | what it shows |
|------|------|---------------|
| `fib32` | dispatch-micro | naive recursive fib(32): tiny blocks, maximal call/ret dispatch pressure — best case for the fast-dispatch track. |
| `sha256` | compute-hot | 5000-iteration scalar hash loop: JIT compile amortizes, near native. |
| `sqlite` | one-shot | in-memory query on real sqlite3: startup-dominated, compile-bound. |
| `lua` | one-shot | Lua script on real lua: startup-dominated, compile-bound. |

The bench also asserts native == interpreter == JIT == expected output, so it
doubles as a correctness gate.
