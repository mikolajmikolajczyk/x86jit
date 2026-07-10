---
id: TASK-196
title: Env-gated perf-map emission for JIT-compiled blocks (X86JIT_PERF_MAP=1)
status: In Progress
assignee: []
created_date: '2026-07-10 16:13'
updated_date: '2026-07-10 16:30'
labels:
  - unemups4-migration
dependencies: []
ordinal: 220000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Let Linux `perf` symbolize compiled guest blocks so embedders (unemups4) can attribute samples to guest RIPs. Standard `/tmp/perf-<pid>.map` convention: one line per symbol, `<hex start> <hex size> <name>\n`, no `0x` prefixes.

Design (decided; implement it):
- New module `x86jit-cranelift/src/perfmap.rs` (NOT x86jit-core — core stays pure-data per the codemap.rs constraint).
- `pub(crate) fn record(start: usize, len: u32, name: &str)`; internally `OnceLock<Option<Mutex<LineWriter<File>>>>`, `Some` iff `X86JIT_PERF_MAP=1`.
- Env off → one `OnceLock` get + `is_none` branch on the cold compile path; zero cost in emitted code.
- Hook site: `compile_with()` in `x86jit-cranelift/src/lib.rs` immediately after `codemap::register(...)` — host entry + code_len already in scope. Guest entry PC is NOT in scope: thread one param through — `compile()` passes `ir.guest_start`, `compile_region()` passes `region.entry`, plus a block/region discriminator.
- Names: `jit_0x<guest_start:x>` and `jit_region_0x<entry:x>`.
- Do NOT use the srcloc table's first entry (guest RIPs truncated to u32 there).
- Background tier-up worker compiles through the same path so it's covered automatically; the `Mutex` serializes fg/bg emission.
- Accepted limitations: entries append-only, never retracted (cranelift-jit never frees code); DWARF unwind doesn't cross JIT frames, JIT samples attribute flat to their block — fine for which-blocks-are-hot.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 `X86JIT_PERF_MAP` unset: no file created, no behavior change, differential corpus green
- [x] #2 `X86JIT_PERF_MAP=1` run produces a well-formed `/tmp/perf-<pid>.map` whose ranges match `codemap::lookup`
- [x] #3 Unit test for the line formatter (formatter takes `&mut impl Write`, testable without touching /tmp)
- [ ] #4 `perf record` on x86jit-bench shows `jit_0x...` symbols in perf report — BLOCKED: `perf` not installed in this environment (and perf_event_paranoid=2). Maintainer runs it (steps in Notes).
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed as designed.

Files:
- New `x86jit-cranelift/src/perfmap.rs`: `pub(crate) fn record(start, len, Kind, guest)` gated on a process-global `OnceLock<Option<Mutex<LineWriter<File>>>>` — `Some` iff `X86JIT_PERF_MAP=1` (env read exactly once, file `/tmp/perf-<pid>.map` opened lazily). `Kind::{Block,Region}` selects the symbol prefix (`jit_0x` / `jit_region_0x`). Line formatting is split into `format_line(&mut impl Write, ...)` for testability. Poisoned-lock and write/open errors degrade to no-op (best-effort diagnostics, never affects execution).
- `x86jit-cranelift/src/lib.rs`: `mod perfmap;`. Threaded the guest entry PC + kind through the compile spine — `compile_with(perf_kind: perfmap::Kind, perf_guest: u64, translate)`; `compile()` passes `(Kind::Block, ir.guest_start)`, `compile_region()` passes `(Kind::Region, region.entry)`. Hook is one line right after `codemap::register(...)`: `perfmap::record(entry.0 as usize, code_len, perf_kind, perf_guest);` — same host range codemap gets, named by the real guest entry (NOT the srcloc table's first entry, whose RIPs are u32-truncated). Background tier-up compiles flow through the same `compile_with`, so bg blocks are covered automatically; the `Mutex` serializes fg/bg lines.

No public API surface change: `record`/`Kind` are `pub(crate)`, the new `compile_with` params are on a private `impl Shared` method.

Testing:
- `perfmap::tests::format_line_*` (3): block line, region line, zero/large-value edges — assert exact bytes incl. no `0x` prefixes and `\n`. Uses `&mut Vec<u8>` (an `impl Write`), no /tmp.
- `tests::perfmap_range_matches_codemap`: compiles a block, confirms `codemap::lookup` within the block's host range resolves to the same guest RIP the perf-map symbol names (AC#2 as a runnable invariant). Note: `codemap::lookup(entry.0)` at the exact entry offset can be `None` (no srcloc at offset 0) and its RIPs are u32-truncated — which is exactly why perfmap names from `ir.guest_start` directly; the test scans a small window forward.

Verification:
- AC#1: full `cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)'` — 479 passed, 2 skipped, 0 failed (env unset by default → no file, no behavior change; differential corpus green).
- AC#2: `X86JIT_PERF_MAP=1 cargo run -p x86jit-bench --release -- record --iters 1 --warmup 0` produced `/tmp/perf-<pid>.map` (600 KB, 20072 lines); 0 lines failed regex `^[0-9a-f]+ [0-9a-f]+ jit(_region)?_0x[0-9a-f]+$`; 0 zero-size entries; 9270 distinct `jit_0x` block syms + 152 `jit_region_0x` region syms. Range/name consistency with codemap also asserted by the unit test above. (Bench artifacts it wrote — performance.md/baseline.json/history — were reverted; not part of this task.)
- AC#3: covered by `format_line_*`.
- AC#4: BLOCKED — `perf` is not installed in this env and perf_event_paranoid=2. Everything else verified. Maintainer steps below.
- DoD#1/2/3: nextest (above) green; `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --all --check` clean (nix-pinned rustfmt).

Maintainer — verify AC#4 with perf:
```sh
# Build the bench release binary (JIT default-on).
nix develop -c cargo build -p x86jit-bench --release
# Sample it while it runs, with the perf map enabled so perf can symbolize JIT code.
sudo sysctl kernel.perf_event_paranoid=1   # or -1; needed to record
X86JIT_PERF_MAP=1 perf record -g -o /tmp/perf.data -- \
  ./target/release/x86jit-bench record --iters 3 --warmup 1
# perf auto-reads /tmp/perf-<pid>.map written by the run.
perf report -i /tmp/perf.data | grep -E 'jit_0x|jit_region_0x'   # expect JIT symbols
```
Expect `jit_0x...` (and `jit_region_0x...`) symbols attributed to the hot guest blocks. DWARF unwind does not cross JIT frames, so samples attribute flat to their block — sufficient for which-blocks-are-hot.

Accepted limitations (also in module docs): perf-map entries are append-only, never retracted (cranelift-jit never frees code; SMC-dropped blocks keep their bytes) — matches codemap's lifetime model. Flat attribution per block, no cross-JIT-frame unwind.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
