#!/usr/bin/env bash

# >>> repoctx (managed — edits here are overwritten) >>>
repoctx prime 2>/dev/null
# <<< repoctx (managed) <<<
# Claude Code SessionStart hook (project-local, optional).
# Prints a quick orientation snapshot so the agent doesn't burn tokens
# rediscovering project state. Wire it in via .claude/settings.json if wanted.

set -u

cd "${CLAUDE_PROJECT_DIR:-$(pwd)}" 2>/dev/null || exit 0

print_section() {
  printf '\n--- %s ---\n' "$1"
}

print_section "branch + last 5 commits"
git log --format="%h %s" -5 2>/dev/null || echo "(no git)"

# --- task backlog lives in wiki/tasks/*.md checkboxes (not a forge) ---
# milestone files first (m0..m8), then cross-cutting tracks; README excluded.
task_files=$(
  ls wiki/tasks/m*.md 2>/dev/null | sort
  ls wiki/tasks/*.md 2>/dev/null | grep -vE '/(README|m[0-9]+-)' | sort
)

print_section "milestone progress (wiki/tasks/*.md)"
if [ -n "$task_files" ]; then
  total_open=0
  total_done=0
  focus=""
  for f in $task_files; do
    o=$(grep -cE '^[[:space:]]*- \[ \]' "$f")
    d=$(grep -cE '^[[:space:]]*- \[x\]' "$f")
    total_open=$((total_open + o))
    total_done=$((total_done + d))
    [ -z "$focus" ] && [ "$o" -gt 0 ] && focus=$(basename "$f" .md)
    printf '  %-24s %d/%d done, %d open\n' "$(basename "$f" .md)" "$d" "$((d + o))" "$o"
  done
  printf '  %-24s %d done, %d open\n' "TOTAL" "$total_done" "$total_open"
  [ -n "$focus" ] && printf '  focus → %s (first milestone with open tasks)\n' "$focus"
else
  echo "(no wiki/tasks/*.md found)"
fi

print_section "next open tasks"
if [ -n "$task_files" ]; then
  # shellcheck disable=SC2086
  grep -hE '^[[:space:]]*- \[ \] \*\*(M[0-9]+-T[0-9]+[a-z]?|INT-T[0-9]+)\*\*' $task_files 2>/dev/null \
    | sed -E 's/^[[:space:]]*- \[ \] //; s/\*\*//g' \
    | head -8
  echo "  ... (full lists in wiki/tasks/, see wiki/tasks/README.md)"
fi

print_section "load order reminder"
cat <<'EOF'
1. AGENTS.md (root) → conventions + pointer table
2. wiki/tasks/README.md → task backlog + ordering (M<n>-T<k>)
3. wiki/tasks/<current-milestone>.md → the open checkboxes to work
4. Read only the wiki/agents/*.md files relevant to the task
5. wiki/design/spec.md → authoritative design spec + milestones
EOF

exit 0
