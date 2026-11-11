# Contributing

How to set up the environment, build Magnolia and run its checks. The rules of the code are in
[`AGENTS.md`](AGENTS.md); its parts in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## The environment

Magnolia builds on Linux, x86_64, inside a Nix development shell (`flake.nix`) that holds every tool
the checks use, pinned by `flake.lock`. CI uses the same shell.

```sh
nix develop
```

It holds:

- the stable Rust toolchain pinned in `rust-toolchain.toml`, which builds Magnolia;
- `nightly`, which runs a command with the pinned nightly toolchain first on the path, for the
  checks that need one: RealtimeSanitizer, Miri and `cargo fuzz`
  ([ADR-0013](docs/adr/0013-a-pinned-stable-toolchain-and-a-nightly-for-checks.md));
- `cargo deny`, which enforces the dependency policy of `deny.toml`;
- `cargo fuzz`, which runs the fuzz targets with libFuzzer on the nightly;
- `lychee`, which checks the links between Markdown files.

The first entry downloads both toolchains, about 1 GB.

## Building and testing

The engine holds the time model so far, a library without a program to run. The commands that
build and test it, and measure its conversions:

```sh
cargo build --workspace
cargo test --workspace
cargo bench --bench time_conversions
```

## Fuzzing

Each crate's fuzz targets live in its `fuzz/` folder, a workspace of its own. To list them, then
run one for a minute:

```sh
nightly cargo fuzz list --fuzz-dir crates/magnolia-engine/fuzz
nightly cargo fuzz run --fuzz-dir crates/magnolia-engine/fuzz --target-dir target/fuzz tempo_map \
    -- -max_total_time=60
```

`--target-dir target/fuzz` shares the builds of `scripts/check.sh`. A crash leaves its input in
`crates/magnolia-engine/fuzz/artifacts/`; `cargo fuzz run` with that file's path after the
target's name replays it. The corpus grows in `fuzz/corpus/`, which git
ignores.

## The checks

```sh
scripts/check.sh
```

runs every automated check of the definition of done, in this order, and stops at the first that
fails. CI runs it unchanged, on each push to `main`, each pull request and once a week.

| Step | What it checks | Alone |
|---|---|---|
| links | Links between Markdown files, anchors included, offline | `lychee --offline --include-fragments $(git ls-files --cached --others --exclude-standard '*.md')` |
| AGENTS.md length | `AGENTS.md` under 1,500 words | `wc -w AGENTS.md` |
| scripts | The scripts' syntax | `for script in scripts/*.sh; do bash -n "$script"; done` |
| formatting | rustfmt's format, the fuzz targets' too | `cargo fmt --all --check` |
| lints | Clippy and the compiler's lints of `Cargo.toml`, warnings as errors, the fuzz targets' too | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| tests | Every test on the stable toolchain | `cargo test --workspace --locked` |
| documentation | rustdoc, warnings as errors | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked` |
| dependencies | Licenses, RustSec advisories and sources, by `deny.toml`, the fuzz targets' too; fetches the advisories | `cargo deny --locked check` |
| RealtimeSanitizer | Clippy on the sanitizer's test, then the sanitizer stops an allocation in a function marked real-time, and stays silent on a block's conversions of time | below |
| fuzzing | Each fuzz target for 20 s, or `MAGNOLIA_FUZZ_SECONDS` | [Fuzzing](#fuzzing) |

The sanitizer's test runs only on the nightly, with the sanitizer's flags, in a target directory of
its own; `cargo test` leaves it out
([ADR-0019](docs/adr/0019-the-sanitizers-nightly-code-behind-a-cfg.md)). `scripts/check.sh` runs
Clippy on it first, with the same arguments, then the test:

```sh
RUSTFLAGS="-Zsanitizer=realtime --cfg magnolia_rtsan" nightly cargo test --locked \
    -p magnolia-engine --test realtime_sanitizer \
    --target x86_64-unknown-linux-gnu --target-dir "${CARGO_TARGET_DIR:-target}/realtime-sanitizer"
```

The fuzz targets' own formatting, lints and dependencies are checked with the same commands, given
`--manifest-path crates/magnolia-engine/fuzz/Cargo.toml`.

How long a full check takes, and its budget, are in
[`docs/measurements.md`](docs/measurements.md#m12--the-time-model).
