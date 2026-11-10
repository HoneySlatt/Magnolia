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
- `cargo fuzz`, for the fuzz targets that arrive with the first parser;
- `lychee`, which checks the links between Markdown files.

The first entry downloads both toolchains, about 1 GB.

## Building and testing

There is nothing to run yet: the engine has no code. The commands that build and test it:

```sh
cargo build --workspace
cargo test --workspace
```

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
| formatting | rustfmt's format | `cargo fmt --all --check` |
| lints | Clippy and the compiler's lints of `Cargo.toml`, warnings as errors | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| tests | Every test on the stable toolchain | `cargo test --workspace --locked` |
| documentation | rustdoc, warnings as errors | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked` |
| dependencies | Licenses, RustSec advisories and sources, by `deny.toml`; fetches the advisories | `cargo deny --locked check` |
| RealtimeSanitizer | Clippy on the sanitizer's test, then the sanitizer stops an allocation in a function marked real-time | below |

The sanitizer's test is built only with the `realtime-sanitizer` feature, on the nightly, in a
target directory of its own. `scripts/check.sh` runs Clippy on it first, with the same arguments,
then the test:

```sh
RUSTFLAGS="-Zsanitizer=realtime" nightly cargo test --locked -p magnolia-engine \
    --features realtime-sanitizer --test realtime_sanitizer \
    --target x86_64-unknown-linux-gnu --target-dir "${CARGO_TARGET_DIR:-target}/realtime-sanitizer"
```

How long a full check takes, and its budget, are in
[`docs/measurements.md`](docs/measurements.md#m11--the-workspace-and-its-checks).
