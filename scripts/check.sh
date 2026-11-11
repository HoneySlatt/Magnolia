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

# The fuzz targets live in a workspace of their own (ADR-0013); their builds share target/.
fuzz=(--manifest-path crates/magnolia-engine/fuzz/Cargo.toml)
fuzz_target_dir="${CARGO_TARGET_DIR:-target}/fuzz"
# How long each fuzz target runs; set it higher to fuzz longer by hand.
fuzz_seconds="${MAGNOLIA_FUZZ_SECONDS:-20}"

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
cargo fmt "${fuzz[@]}" --check

step "lints"
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy "${fuzz[@]}" --locked --target-dir "$fuzz_target_dir" -- -D warnings

step "tests"
cargo test --workspace --locked

step "documentation"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

step "dependencies (licenses, advisories, sources)"
cargo deny --locked check
cargo deny "${fuzz[@]}" --locked check

# RealtimeSanitizer ships with the nightly's standard library (ADR-0013). `--target` keeps the flag
# off build scripts and procedural macros, as the Unstable Book asks; a target directory of its own
# keeps these builds from evicting the stable ones.
step "RealtimeSanitizer stops an allocation in a function marked real-time, passes a block's conversions"
sanitized=(--locked -p magnolia-engine --features realtime-sanitizer --test realtime_sanitizer
    --target x86_64-unknown-linux-gnu --target-dir "${CARGO_TARGET_DIR:-target}/realtime-sanitizer")
nightly cargo clippy "${sanitized[@]}" -- -D warnings
RUSTFLAGS="-Zsanitizer=realtime" nightly cargo test "${sanitized[@]}"

# cargo fuzz builds with the nightly, libFuzzer and AddressSanitizer; a crash fails the step and
# leaves its input in crates/magnolia-engine/fuzz/artifacts/. It takes no --locked: the fuzz crate's
# Clippy above has checked its lock file.
step "fuzzing, ${fuzz_seconds} s per target"
# The list is read apart: a command that fails inside a `for` list would not stop the script.
fuzz_targets=$(nightly cargo fuzz list --fuzz-dir crates/magnolia-engine/fuzz)
if [ -z "$fuzz_targets" ]; then
    echo "cargo fuzz lists no fuzz target in crates/magnolia-engine/fuzz" >&2
    exit 1
fi
for target in $fuzz_targets; do
    nightly cargo fuzz run --fuzz-dir crates/magnolia-engine/fuzz --target-dir "$fuzz_target_dir" \
        "$target" -- -max_total_time="$fuzz_seconds"
done

step "all checks passed"
