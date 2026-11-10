#!/usr/bin/env bash
# Every automated check of the definition of done (AGENTS.md), which CI runs unchanged inside the
# development shell (flake.nix): `nix develop --command scripts/check.sh`. CONTRIBUTING.md says what
# each step checks and how to run one alone.
set -euo pipefail
cd "$(dirname "$0")/.."

# The shell provides `nightly` (flake.nix); without it, the sanitizer's step would fail only after
# minutes of other checks.
command -v nightly >/dev/null || {
    echo "run this script inside the development shell: nix develop --command scripts/check.sh" >&2
    exit 1
}

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

step "formatting"
cargo fmt --all --check

step "lints"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "tests"
cargo test --workspace --locked

step "documentation"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

step "dependencies (licenses, advisories, sources)"
cargo deny --locked check

# RealtimeSanitizer ships with the nightly's standard library (ADR-0013). `--target` keeps the flag
# off build scripts and procedural macros, as the Unstable Book asks; a target directory of its own
# keeps these builds from evicting the stable ones.
step "RealtimeSanitizer stops an allocation in a function marked real-time"
sanitized=(--locked -p magnolia-engine --features realtime-sanitizer --test realtime_sanitizer
    --target x86_64-unknown-linux-gnu --target-dir "${CARGO_TARGET_DIR:-target}/realtime-sanitizer")
nightly cargo clippy "${sanitized[@]}" -- -D warnings
RUSTFLAGS="-Zsanitizer=realtime" nightly cargo test "${sanitized[@]}"

step "all checks passed"
