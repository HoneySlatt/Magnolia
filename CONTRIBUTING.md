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
- `lychee`, which checks the links between Markdown files;
- JACK2: its libjack, which the JACK backend loads at run time (the shell puts it on
  `LD_LIBRARY_PATH`), and `jackd`, whose dummy driver runs the backend's tests without a sound
  card.

The first entry downloads both toolchains, about 1 GB.

## Building and testing

The engine is a library; the JACK backend plays it, and its example plays a sine. The commands that
build and test them, and measure the time model's conversions:

```sh
cargo build --workspace
cargo test --workspace
cargo bench --bench time_conversions
```

### The JACK backend's tests

`cargo test -p magnolia-jack` runs the backend's tests, each on a JACK2 server of its own that the
test starts and stops by its pid: `jackd -n magnolia-test-<pid>-<n> -r -d dummy -r 48000 -p
<period>`, without real time (`-r` before `-d`) and without a sound card. They run one at a time.
They need `jackd` and JACK2's libjack, both in the development shell, and they refuse to run, with
the reason, when the libjack they would load is PipeWire's, as under `pw-jack`: a client would then
talk to the live PipeWire session instead of the test's own server. A JACK2 server that dies
without stopping cleanly keeps its place in JACK2's registry of servers, which holds eight, and its
shared memory in `/dev/shm`; once eight places are held so, no server starts. So after killing its
server, a test starts one under the same name and stops it cleanly, which frees both, and removes
the socket and semaphores named after that server (`tests/common/mod.rs`).

### Playing through JACK

`examples/play.rs` plays a sine at -20 dBFS through the backend, crossfading to another sine every
two seconds, then prints whether the audio thread ran in real time and the engine's figures. On
PipeWire, through `pw-jack`, at a period of 64 frames, connected to the sound card:

```sh
pw-jack -p 64 cargo run --release -p magnolia-jack --example play -- --connect
```

Its options: `--seconds N` (10 by default), `--server NAME` (the default server otherwise),
`--connect` (connect to the first physical playback ports; off by default), `--swap-every SECONDS`
(2 by default, 0 never), `--load N` (N chains of a sine and a gain, summed below -40 dBFS, as a
synthetic load). On a JACK2 server of one's own, without a sound card:

```sh
jackd -n magnolia-try -r -d dummy -r 48000 -p 256 &
cargo run --release -p magnolia-jack --example play -- --server magnolia-try --load 64
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
| lints | Clippy and the compiler's lints of `Cargo.toml`, warnings as errors, the fuzz targets' too; `clippy.toml` forbids std's locks and blocking waits ([ADR-0020](docs/adr/0020-clippy-forbids-the-locks-the-sanitizer-cannot-see.md)) | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| tests | Every test on the stable toolchain | `cargo test --workspace --locked` |
| page faults in release | A swap takes no page fault on the processing thread in an optimised build, where a zero fill may become `calloc` | `cargo test --release --locked -p magnolia-engine --test engine page_faults` |
| documentation | rustdoc, warnings as errors | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked` |
| dependencies | Licenses, RustSec advisories and sources, by `deny.toml`, the fuzz targets' too; fetches the advisories | `cargo deny --locked check` |
| RealtimeSanitizer | Clippy on each crate's sanitizer test, then the sanitizer stops an allocation and a lock's wait in functions marked real-time, lets the collector's wake pass by its suppression, and stays silent on a block's conversions of time, on the engine's processor and on the JACK backend's process callback, played on a dummy JACK2 server | below |
| fuzzing | Each fuzz target for 20 s, or `MAGNOLIA_FUZZ_SECONDS` | [Fuzzing](#fuzzing) |

The sanitizer's tests, one in `magnolia-engine` and one in `magnolia-jack`, run only on the
nightly, with the sanitizer's flags, in a target directory of their own; `cargo test` leaves them
out ([ADR-0019](docs/adr/0019-the-sanitizers-nightly-code-behind-a-cfg.md)). `scripts/check.sh`
runs Clippy on each first, with the same arguments, then the test; for the engine:

```sh
RUSTFLAGS="-Zsanitizer=realtime --cfg magnolia_rtsan" nightly cargo test --locked \
    -p magnolia-engine --test realtime_sanitizer \
    --target x86_64-unknown-linux-gnu --target-dir "${CARGO_TARGET_DIR:-target}/realtime-sanitizer"
```

and the same with `-p magnolia-jack` for the backend, whose test starts a JACK2 server of its own.

Each test runs each case in a child process and sets the sanitizer's options for it:

```sh
RTSAN_OPTIONS=halt_on_error=true:allow_addr2line=true:suppressions=<workspace>/rtsan-suppressions.txt
```

`halt_on_error` ends the child at the first report. `allow_addr2line` symbolizes the reports with
binutils' `addr2line`, which the development shell provides, so that the suppressions see function
names. `rtsan-suppressions.txt` names the functions that make the calls of the real-time allowlist
([ADR-0023](docs/adr/0023-the-real-time-allowlist.md)). One of the engine's cases runs without
the suppressions, to show that the collector's wake is a system call the sanitizer stops. To read a
case's report, start the test's executable with `MAGNOLIA_RTSAN_CASE` set to the case's name and
the options above; the backend's `backend` case also needs `MAGNOLIA_RTSAN_SERVER`, the name of a
running JACK2 server.

The fuzz targets' own formatting, lints and dependencies are checked with the same commands, given
`--manifest-path crates/magnolia-engine/fuzz/Cargo.toml`.

How long a full check takes, and its budget, are in
[`docs/measurements.md`](docs/measurements.md#m12--the-time-model).
