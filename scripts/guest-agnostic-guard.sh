#!/usr/bin/env bash
# Refuse engine source that names a downstream consumer.
#
# x86jit is a guest-agnostic x86-64 engine: it knows nothing about PS4, about any
# particular game, or about the emulator embedding it. That is the project's central
# claim, and it is the kind of claim that erodes one comment at a time — a lift driven
# by a downstream trap gets a note saying which game hit it, and a year later the
# "guest-agnostic" engine is full of Celeste.
#
# It has happened: agents working a lift on a downstream project's behalf have left
# game names, guest runtime names and that project's task ids in engine source. A scan
# today is clean; nothing but this script stops it coming back.
#
#   scripts/guest-agnostic-guard.sh          # scan tracked Rust source
#
# Where a downstream reference IS legitimate — a backlog task recording who reported a
# bug, or PROVENANCE naming a consumer — it belongs in backlog/ or a top-level document,
# which this deliberately does not scan.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Downstream consumers, their guests, and their runtimes. Extend it when a new
# consumer appears — the cost of a false positive is renaming one comment; the cost of
# a miss is the engine quietly acquiring a guest.
DENY=(
  celeste "little nightmares" doom quake
  ps4 ps5 playstation orbis liverpool
  unemups4
  mono monogame sgen libfmod xna
  homebrew
)

# NOT on the list, deliberately:
#   unemulinux — the sibling embedder, not a guest. Naming it is correct and in places
#     required: boundary.rs exists to say what belongs there rather than here, and
#     several doc comments point at its design documents. A rule that forbade it would
#     forbid explaining the boundary.
#   fmod — C's floating-point remainder, which f80.rs legitimately cites. The leak
#     shape is `libfmod`, the audio library, so match that instead.
#   pkg — appears in `pkg-config` and is too generic to earn its place.
#
# Matched as whole words because they occur inside ordinary identifiers and English:
# `xna` inside `xnat()`, `mono` inside `monotonic`, `doom`/`quake` in prose.
WORD_ONLY=" mono doom quake homebrew xna "

files=$(git ls-files '*.rs' | grep -v '^backlog/' || true)
[[ -n "$files" ]] || { echo "no tracked Rust source — nothing to scan"; exit 0; }

rc=0
for term in "${DENY[@]}"; do
  if [[ "$WORD_ONLY" == *" $term "* ]]; then
    pattern="\\b${term}\\b"
  else
    pattern="$term"
  fi
  # -I skips binaries, -n gives the line, -i because a comment may capitalise it.
  # shellcheck disable=SC2086  # $files is a deliberate word-split list of paths.
  if hits=$(grep -rIniE --color=never "$pattern" $files 2>/dev/null); then
    echo "guest-agnostic guard: engine source names a downstream consumer ('$term'):"
    # shellcheck disable=SC2001  # indenting every line of a multi-line block; the
    # parameter-expansion form does not do per-line replacement.
    echo "$hits" | sed 's/^/  /'
    rc=1
  fi
done

if (( rc )); then
  cat >&2 <<'EOF'

x86jit is guest-agnostic: no downstream project, guest, game or runtime may be named
in engine source. Record it in the backlog task instead — that is where "who reported
this" belongs, and it is not scanned.

If the term is a false positive (a crate name, an English word), add it to WORD_ONLY
or drop it from DENY in scripts/guest-agnostic-guard.sh, with a comment saying why.
EOF
fi
exit $rc
