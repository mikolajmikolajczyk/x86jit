#!/usr/bin/env bash
# Fetch the TomHarte / SingleStepTests 8088 per-instruction corpus (by Daniel
# Balsom & Folkert van Heusden) into this directory. The JSON is large (~800 MB
# uncompressed, ~250 MB gzip across 325 per-opcode files) so it is gitignored and
# fetched on demand — locally and in CI — rather than vendored.
#
# Layout after a run:
#   vendor/8088/v1/8088.json          — opcode metadata (status + undefined-flag masks)
#   vendor/8088/v1/XX.json.gz         — 10 000 tests for opcode 0xXX
#   vendor/8088/v1/XX.R.json.gz       — modrm-reg-extended groups (80/81/83/D0-D3/F6/F7/FE/FF)
#
# The loader (src/harte.rs) decompresses each `.json.gz` on load; there is no need
# to gunzip them. Pass opcode hex strings as args to fetch only a subset, e.g.
#   ./fetch.sh 00 01 D0.4
# With no args it fetches the full corpus.
set -euo pipefail

BASE="https://raw.githubusercontent.com/SingleStepTests/ProcessorTests/main/8088/v1"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/v1"
mkdir -p "$DIR"

# Opcode metadata (undefined-flag masks) — always fetched.
curl -sfL "$BASE/8088.json" -o "$DIR/8088.json"

if [[ $# -gt 0 ]]; then
    OPS=("$@")
else
    # Full corpus: 0x00-0xFF, with the modrm-reg groups expanded to XX.0..XX.7
    # (or XX.0..XX.1 for FE). The server 404s on absent members; we skip those.
    OPS=()
    for hi in 0 1 2 3 4 5 6 7 8 9 A B C D E F; do
        for lo in 0 1 2 3 4 5 6 7 8 9 A B C D E F; do
            OPS+=("${hi}${lo}")
        done
    done
    for grp in 80 81 83 D0 D1 D2 D3 F6 F7 FF; do
        for r in 0 1 2 3 4 5 6 7; do OPS+=("${grp}.${r}"); done
    done
    OPS+=(FE.0 FE.1)
fi

n=0
for op in "${OPS[@]}"; do
    f="${op}.json.gz"
    if curl -sfL "$BASE/$f" -o "$DIR/$f"; then
        n=$((n + 1))
    else
        rm -f "$DIR/$f" # 404 (grouped opcode has its own XX.R files instead)
    fi
done
echo "fetched $n opcode files into $DIR"
