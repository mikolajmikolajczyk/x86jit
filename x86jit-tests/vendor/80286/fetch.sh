#!/usr/bin/env bash
# Fetch the SingleStepTests **80286** real-mode per-instruction corpus (by Daniel
# Balsom, captured from a real Harris N80C286-12 via the ArduinoX86 interface) into
# this directory. The corpus is large (~1.5 M executions across 326 instruction
# forms) so it is gitignored and fetched on demand — locally and in CI — rather
# than vendored.
#
# Layout after a run:
#   vendor/80286/v1_real_mode/metadata.json   — opcode metadata (status + undefined-flag masks)
#   vendor/80286/v1_real_mode/XX.MOO.gz        — tests for opcode 0xXX (MOO binary, gzip)
#   vendor/80286/v1_real_mode/XX.R.MOO.gz      — modrm-reg groups (80/81/82/83/C0/C1/D0-D3/F6/F7/FE/FF)
#
# Format is MOO (a simple chunked binary; https://github.com/dbalsom/moo). The loader
# (src/ss286.rs) decompresses each `.MOO.gz` and parses the MOO chunks directly in
# Rust — no JSON conversion step, no Python dependency (the cycle traces we do not
# model are skipped chunk-by-chunk, so parsing is cheap). Pass opcode hex stems as
# args to fetch only a subset, e.g.
#   ./fetch.sh 00 01 F7.6
# With no args it fetches the full real-mode corpus.
set -euo pipefail

BASE="https://raw.githubusercontent.com/SingleStepTests/80286/main/v1_real_mode"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/v1_real_mode"
mkdir -p "$DIR"

# Opcode metadata (undefined-flag masks) — always fetched.
curl -sfL "$BASE/metadata.json" -o "$DIR/metadata.json"

if [[ $# -gt 0 ]]; then
    OPS=("$@")
else
    # Full corpus: 0x00-0xFF plain, plus the modrm-reg groups expanded to XX.0..XX.7.
    # The server 404s on absent members (e.g. prefix bytes, undefined opcodes,
    # FE.2..FE.7, FF.7); we detect and skip those.
    OPS=()
    for hi in 0 1 2 3 4 5 6 7 8 9 A B C D E F; do
        for lo in 0 1 2 3 4 5 6 7 8 9 A B C D E F; do
            OPS+=("${hi}${lo}")
        done
    done
    for grp in 80 81 82 83 C0 C1 D0 D1 D2 D3 F6 F7 FE FF; do
        for r in 0 1 2 3 4 5 6 7; do OPS+=("${grp}.${r}"); done
    done
fi

n=0
for op in "${OPS[@]}"; do
    f="${op}.MOO.gz"
    if curl -sfL "$BASE/$f" -o "$DIR/$f"; then
        n=$((n + 1))
    else
        rm -f "$DIR/$f" # 404 (prefix byte, undefined opcode, or grouped-only opcode)
    fi
done
echo "fetched $n opcode files into $DIR"
