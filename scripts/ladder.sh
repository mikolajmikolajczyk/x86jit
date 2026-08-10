#!/usr/bin/env bash
# Run unemulinux's real-program ladder against this recompiler.
#
# The ISA corpus in this repository tells you what is wrong with the instructions
# that ARE lifted. It cannot tell you what a real program trips over — that is what
# the ladder is for, and since the Linux userland split it lives in a different
# repository. This script is the local half of that gate (task-236); the CI half is
# unemulinux's `repository_dispatch`.
#
#   scripts/ladder.sh                  # smoke subset against the working tree
#   scripts/ladder.sh --full           # the whole ladder (~10 min)
#   scripts/ladder.sh --rev HEAD~5     # against a specific revision, in a worktree
#                                      # (the userland is always the working tree)
#   scripts/ladder.sh --full -- -E 'test(caddy)'   # extra nextest args after --
#   scripts/ladder.sh --if-present     # for hooks: skip loudly when unemulinux is absent
#
# unemulinux is found at $UNEMULINUX_DIR, or ../unemulinux next to this checkout.
set -euo pipefail

x86jit="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
unemulinux="${UNEMULINUX_DIR:-$(dirname "$x86jit")/unemulinux}"

# The smoke subset task-236 blesses as a first step: one static musl program, one
# dynamic glibc program, one Go binary. Each covers a different class of failure —
# a bare lift, a dynamic loader's relocation processing, and the Go runtime's
# threading and memory reservations — so passing all three is meaningfully more
# than passing any one of them.
SMOKE='test(musl_hello_native_interp_jit_agree) + test(glibc_hello_native_interp_jit_agree) + binary(go_hello) + test(busybox_wc_native_interp_jit_agree)'

mode=smoke
rev=""
if_present=0
extra=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --full)  mode=full; shift ;;
    --smoke) mode=smoke; shift ;;
    --rev)   rev="${2:?--rev needs a git ref}"; shift 2 ;;
    # For the pre-push hook only. A contributor without an unemulinux checkout should
    # not be unable to push; a contributor who HAS one should not be able to skip the
    # gate by accident. Hence two behaviours, and the skip is printed, never silent.
    --if-present) if_present=1; shift ;;
    --)      shift; extra=("$@"); break ;;
    -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ -d "$unemulinux/.git" ]] || {
  if [[ "$if_present" == 1 ]]; then
    echo "SKIP: the real-program ladder did not run — unemulinux is not at $unemulinux." >&2
    echo "      ISA-level tests alone cannot tell you what real software trips over." >&2
    echo "      git clone https://github.com/unemu-org/unemulinux $unemulinux" >&2
    exit 0
  fi
  echo "error: unemulinux not found at $unemulinux" >&2
  echo "       clone it, or point UNEMULINUX_DIR at it:" >&2
  echo "       git clone https://github.com/unemu-org/unemulinux $unemulinux" >&2
  # Exit 2, never 0: 'the ladder did not run' must not look like 'the ladder passed'.
  exit 2
}

scratch=""
# shellcheck disable=SC2329  # invoked by the EXIT trap below, not by name.
cleanup() {
  [[ -n "$scratch" ]] || return 0
  git -C "$x86jit" worktree remove --force "$scratch/x86jit" 2>/dev/null || true
  rm -rf "$scratch"
}
trap cleanup EXIT

if [[ -n "$rev" ]]; then
  # A path dependency resolves to a directory, so testing a revision other than the
  # working tree means giving it a different directory — hence two worktrees and one
  # rewritten path. Cargo's `paths` override would avoid the second worktree, but it
  # warns that it "is known to produce buggy behavior" and is slated to become a hard
  # error; a gate built on that is not a gate.
  # Beside the checkouts, not in $TMPDIR: the copy below hardlinks, and a hardlink
  # cannot cross a filesystem — /tmp is usually a tmpfs, and a 130 MB fixture set is
  # not something to put in RAM anyway.
  scratch="$(mktemp -d "$(dirname "$unemulinux")/.x86jit-ladder.XXXXXX")"
  sha="$(git -C "$x86jit" rev-parse --short "$rev")"
  tested="x86jit $sha"
  echo "==> x86jit @ $sha ($rev), in a detached worktree"
  git -C "$x86jit" worktree add -q --detach "$scratch/x86jit" "$rev"

  # unemulinux comes from the WORKING TREE, hardlinked, not from a git worktree.
  # Two reasons, both learned the hard way: a git worktree of HEAD misses uncommitted
  # harness changes (so a fix and the ladder that exercises it cannot be tested
  # together), and the guest fixtures are gitignored, so a fresh worktree has none of
  # them — which the `require` policy correctly turns into a hard error. `--rev` pins
  # the *recompiler*; the userland is whatever is on disk. That is the interesting
  # axis, and the summary below says so.
  mkdir -p "$scratch/unemulinux"
  shopt -s dotglob nullglob
  for e in "$unemulinux"/*; do
    b="$(basename "$e")"
    [[ "$b" == target || "$b" == .git ]] && continue
    cp -al "$e" "$scratch/unemulinux/$b" 2>/dev/null ||
      cp -a "$e" "$scratch/unemulinux/$b"
  done
  shopt -u dotglob nullglob

  # Replace rather than edit in place: the manifest is a hardlink to the real one.
  m="$scratch/unemulinux/Cargo.toml"
  sed "s|path = \"../x86jit/|path = \"$scratch/x86jit/|g" "$m" > "$m.new"
  mv -f "$m.new" "$m"
  run_in="$scratch/unemulinux"
else
  sha="$(git -C "$x86jit" rev-parse --short HEAD)"
  dirty="$(git -C "$x86jit" status --porcelain | wc -l)"
  if [[ "$dirty" -gt 0 ]]; then
    # Say it. A ladder run reported against a bare SHA that was not what actually ran
    # is worse than no run at all, because it looks like evidence.
    echo "==> x86jit @ $sha + $dirty uncommitted file(s) — the WORKING TREE, not $sha"
    tested="the working tree ($sha + $dirty uncommitted)"
  else
    echo "==> x86jit @ $sha (clean working tree)"
    tested="x86jit $sha"
  fi
  run_in="$unemulinux"
fi

if [[ "$mode" == smoke ]]; then
  set -- --workspace --no-fail-fast -E "$SMOKE" "${extra[@]}"
else
  set -- --workspace --no-fail-fast "${extra[@]}"
fi

echo "==> ladder: $mode, in $run_in"
cd "$run_in"
# `require` is already the default; setting it here is the point. The one thing this
# script must never do is report green over a ladder that silently lost rungs.
UNEMULINUX_FIXTURES=require RUST_BACKTRACE=1 cargo nextest run "$@"
rc=$?

echo
if [[ "$mode" == smoke ]]; then
  echo "==> SMOKE subset passed against $tested."
  echo "    Covered: static musl, dynamic glibc, a Go binary, one busybox applet."
  echo "    NOT covered: sqlite, lua, CPython, djpeg, caddy, the OCI/CLI rungs,"
  echo "    threading and the multi-process paths. Run --full before you believe it."
else
  echo "==> FULL ladder passed against $tested."
fi
exit $rc
