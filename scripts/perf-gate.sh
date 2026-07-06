#!/usr/bin/env bash
# Pre-push performance regression gate.
#
# Measures HEAD (release) and blocks the push if any workload's interpreter or JIT
# time is more than X86JIT_PERF_THRESHOLD percent (default 10) slower than the
# committed bench/baseline.json.
#
#   override a genuine/accepted regression:  X86JIT_ALLOW_PERF_REGRESSION=1 git push
#   move the baseline (accept new numbers):  cargo run -p x86jit-bench --release -- record
#                                            git add bench/baseline.json backlog/docs/performance.md
#
# No baseline yet (fresh clone) → the gate skips. Different host → skips (timings
# aren't comparable across machines).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
exec cargo run -q -p x86jit-bench --release -- gate
