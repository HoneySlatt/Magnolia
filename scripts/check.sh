#!/usr/bin/env bash
# Every automated check of the definition of done (AGENTS.md), which CI runs unchanged inside the
# development shell (flake.nix). Today the repository holds documents only: the links between
# Markdown files, anchors included, offline, and AGENTS.md under its 1,500 words. The Rust checks
# arrive with the workspace (M1.1 of docs/PLAN.md).
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

step "links"
mapfile -t markdown < <(git ls-files --cached --others --exclude-standard '*.md')
lychee --offline --include-fragments --no-progress "${markdown[@]}"

step "AGENTS.md length"
words=$(wc -w < AGENTS.md)
if [ "$words" -gt 1500 ]; then
    echo "AGENTS.md has $words words; keep it under 1,500 (docs: project method)." >&2
    exit 1
fi

step "scripts"
for script in scripts/*.sh; do
    bash -n "$script"
done

step "all checks passed"
