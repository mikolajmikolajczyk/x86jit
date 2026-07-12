#!/usr/bin/env bash
# lockstep — interpreter-vs-real-hardware differential tracer for x86jit.
#
# Hunts bugs where the interpreter and the JIT agree with each other but BOTH
# differ from a real x86-64 CPU — so jit==interp differential tests can't see them
# (e.g. the vzeroall low-lane and 16-bit movbe bugs). The interpreter records the
# full architectural effect of every instruction it runs into a trace file; a replay
# harness re-runs each one on the host CPU from the captured pre-state and reports the
# first instruction whose result diverges from hardware.
#
# Two steps:
#
#   # 1. Capture a trace while running some x86jit-cli invocation under the interpreter:
#   scripts/lockstep.sh capture -- \
#     ./target/release/x86jit-cli --backend interp --cpu v4 --entropy host \
#     /usr/bin/openssl dgst -sha256 -sign key.pem -out /tmp/sig data.bin
#
#   # 2. Replay it against the host CPU (auto-sharded across cores); prints the first
#   #    divergence, or "no divergence":
#   scripts/lockstep.sh replay
#
# The capture MUST run under `--backend interp` (the hook lives in the interpreter;
# JIT'd blocks bypass it). Traces are large (tens of GB) and land on real disk, not
# tmpfs — override with --out / $LOCKSTEP_TRACE.
#
# Blind spots (a clean replay does NOT prove these correct):
#   - Control flow: branches/calls/rets aren't traced, so a wrong-branch bug is
#     invisible (each op is replayed from its own captured pre-state).
#   - Masked EVEX (k-register operands) and FS/GS-relative memory: the native stub
#     can't set them up, so those ops are skipped.
#   - Flags: comparison is opt-in (--flags) and noisy — the interpreter elides dead
#     flags, so a post-op flag snapshot legitimately differs from hardware.
#
# See backlog/docs/design/lockstep-tracer.md for the full method and rationale.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

TRACE="${LOCKSTEP_TRACE:-${XDG_CACHE_HOME:-$HOME/.cache}/x86jit-lockstep.bin}"

die() {
  echo "lockstep: $*" >&2
  exit 1
}

usage() {
  sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

# ---------------------------------------------------------------------------
# capture — run a command with the interpreter's lockstep capture enabled.
# ---------------------------------------------------------------------------
cmd_capture() {
  local max=20000000 lo=0x0 hi=0xffffffffffffffff
  while [ $# -gt 0 ]; do
    case "$1" in
      --out) TRACE="$2"; shift 2 ;;
      --max) max="$2"; shift 2 ;;    # stop capturing after N records (0 = unbounded)
      --lo) lo="$2"; shift 2 ;;      # restrict capture to guest addresses [lo, hi)
      --hi) hi="$2"; shift 2 ;;
      --) shift; break ;;
      -h|--help) usage 0 ;;
      *) die "unknown capture flag: $1 (did you forget '--' before the command?)" ;;
    esac
  done
  [ $# -gt 0 ] || die "capture: no command given after '--'"

  mkdir -p "$(dirname "$TRACE")"
  rm -f "$TRACE"
  echo "lockstep: capturing to $TRACE (window [$lo,$hi), cap $max records)" >&2
  echo "lockstep: running: $*" >&2
  local envs=(X86JIT_LOCKSTEP="$TRACE" X86JIT_LOCKSTEP_LO="$lo" X86JIT_LOCKSTEP_HI="$hi")
  [ "$max" != 0 ] && envs+=(X86JIT_LOCKSTEP_MAX="$max")
  env "${envs[@]}" "$@" || echo "lockstep: (command exited nonzero — trace may still be usable)" >&2
  local sz
  sz=$(stat -c%s "$TRACE" 2>/dev/null || echo 0)
  echo "lockstep: trace is $(numfmt --to=iec "$sz" 2>/dev/null || echo "$sz B"). Replay with: scripts/lockstep.sh replay" >&2
}

# ---------------------------------------------------------------------------
# replay — re-run each captured op on the host CPU; report the first divergence.
# ---------------------------------------------------------------------------
cmd_replay() {
  local shards flags=0
  shards=$(nproc); [ "$shards" -gt 16 ] && shards=16
  while [ $# -gt 0 ]; do
    case "$1" in
      --shards) shards="$2"; shift 2 ;;
      --flags) flags=1; shift ;;     # also compare arithmetic flags (noisy — see header)
      -h|--help) usage 0 ;;
      *) TRACE="$1"; shift ;;        # positional: trace path
    esac
  done
  [ -s "$TRACE" ] || die "no trace at $TRACE (run 'capture' first, or pass a path / set \$LOCKSTEP_TRACE)"

  echo "lockstep: building the replay harness…" >&2
  cargo build --release -p x86jit-tests --tests >&2
  local bin
  bin=$(cargo test --release -p x86jit-tests --lib --no-run --message-format=json 2>/dev/null \
    | jq -r 'select(.executable != null and .target.name == "x86jit_tests" and (.target.kind[]? == "lib")) | .executable' \
    | tail -1)
  [ -n "$bin" ] && [ -x "$bin" ] || die "could not resolve the x86jit-tests lib-test binary"

  local logs
  logs=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$logs'; pkill -P $$ -f replay_lockstep_trace 2>/dev/null || true" EXIT

  echo "lockstep: replaying $TRACE across $shards shards$([ "$flags" = 1 ] && echo ' (with flags)')…" >&2
  local i
  for i in $(seq 0 $((shards - 1))); do
    local envs=(X86JIT_LOCKSTEP_REPLAY="$TRACE" X86JIT_LOCKSTEP_SHARDS="$shards" X86JIT_LOCKSTEP_SHARD="$i")
    [ "$flags" = 1 ] && envs+=(X86JIT_LOCKSTEP_FLAGS=1)
    env "${envs[@]}" "$bin" replay_lockstep_trace --ignored --nocapture >"$logs/shard-$i.log" 2>&1 &
  done

  # Poll: stop as soon as any shard reports a divergence; else wait for all to finish.
  while pgrep -P $$ -f replay_lockstep_trace >/dev/null; do
    if grep -lq DIVERGENCE "$logs"/shard-*.log 2>/dev/null; then
      pkill -P $$ -f replay_lockstep_trace 2>/dev/null || true
      break
    fi
    sleep 3
  done
  wait 2>/dev/null || true

  # Report the EARLIEST divergence across shards (smallest global "scanned" index).
  local best_scan="" best_block=""
  for f in "$logs"/shard-*.log; do
    grep -q DIVERGENCE "$f" || continue
    local block scan
    block=$(sed -n '/DIVERGENCE/,/scanned)/p' "$f")
    scan=$(printf '%s\n' "$block" | grep -oE '[0-9]+ scanned' | grep -oE '^[0-9]+' | head -1)
    if [ -n "$scan" ] && { [ -z "$best_scan" ] || [ "$scan" -lt "$best_scan" ]; }; then
      best_scan="$scan"; best_block="$block"
    fi
  done

  if [ -n "$best_block" ]; then
    echo "=========================================================================="
    echo "FIRST DIVERGENCE (interp vs hardware):"
    echo "$best_block"
    echo "=========================================================================="
    return 1
  fi
  grep -h 'no divergence' "$logs"/shard-0.log 2>/dev/null || echo "lockstep: no divergence found."
}

case "${1:-}" in
  capture) shift; cmd_capture "$@" ;;
  replay)  shift; cmd_replay "$@" ;;
  -h|--help|"") usage 0 ;;
  *) die "unknown subcommand: $1 (expected 'capture' or 'replay')" ;;
esac
