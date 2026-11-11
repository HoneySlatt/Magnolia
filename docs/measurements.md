# Measurements

The figures measured against the [quality bar](ROADMAP.md#quality-bar), newest milestone last. Each
figure names its machine, its tools' versions, its date and how it was taken, so that anyone can
take it again.

## M1.1 — The workspace and its checks

Taken on 2026-11-11 on the high tier (AMD Ryzen 7 5800X, 8 cores), its load average about 5, with
Nix 2.34.9, rustc 1.99.1 and cargo-deny 0.20.2. The nightly is the one `flake.nix` pins as
2026-11-06; rustc reports it as 1.101.0 of 2026-11-05, the date of its last commit. Times are wall
time from `time`, each taken three times; the figure is the median.

| What | Time | How |
|---|---|---|
| Entering the development shell, already built | 0.15 s | `nix develop --command true` |
| A full check, `target/` removed first | 2.6 s | `rm -rf target; nix develop --command scripts/check.sh` |
| A full check, nothing changed | 1.5 s | `nix develop --command scripts/check.sh` |
| `cargo deny`, fetching the advisories | 1.0 s | `nix develop --command cargo deny --locked check` |
| `cargo deny`, without fetching | 0.6 s | the same with `--disable-fetch` |
| The CI job | 46 s | the job's time on GitHub, run 42919007937 of commit `97eaf72`, one run |

The workspace holds one crate without dependencies, so these figures say little yet. Fetching the
advisories is most of a check with nothing changed. The budget is in the
[quality bar](ROADMAP.md#floors): a full check from a clean build within 3 minutes on the high
tier, the compiling budget of a build from nothing
([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).

## M1.2 — The time model

Taken on 2026-11-11 on the high tier (AMD Ryzen 7 5800X, 8 cores, 16 threads), with rustc 1.99.1,
the nightly rustc 1.101.0 of 2026-11-05, cargo-deny 0.20.2 and cargo-fuzz 0.13.2. Other work ran on
the machine: its load average was between 4 and 36, noted with each table. Each figure is the
median of three runs.

### A conversion, as the engine calls it per block

`cargo bench --bench time_conversions` (`crates/magnolia-engine/benches/`), at 48 kHz, blocks of 64
frames with 16 events each. The positions are spread over ten hours, and so are the tempo changes.
Each run is itself the median of nine timed loops.

The machine was busy with other work: load average 34 to 36 on 16 threads. These figures are
therefore upper bounds. An earlier version of the benchmark ran on a quiet machine (load 3.3) and
gave about half of them, but most of its positions fell past the last tempo change. The quiet
measure of this version is still to be taken.

| What | 1 tempo change | 1,000 tempo changes |
|---|---|---|
| A tick placed on its sample | 26.6 ns | 49.2 ns |
| A sample read as a tick | 18.7 ns | 34.7 ns |
| A block: its ticks, then its 16 events placed | 403 ns | 906 ns |

A block of 64 frames at 48 kHz lasts 1.33 ms; even under this load, its conversions take under
0.07 % of it. The cost grows with the number of tempo changes through the binary search alone. No
budget is set for it: the [callback's](ROADMAP.md#floors) budget holds it.

### Compiling and checking

`time` on each command, inside the development shell; "from nothing" removes `target/` first. Load
average between 4 and 16.

| What | Time | Budget ([floors](ROADMAP.md#floors)) |
|---|---|---|
| A debug build from nothing | 0.72 s | 3 min |
| A debug build after a change to the time model | 0.37 s | 10 s |
| The tests' build after a change to the time model | 0.41 s | — |
| A release build from nothing | 0.80 s | 10 min |
| A full check from nothing | 84 s | 3 min |
| A full check, nothing changed | 49 s | — |

These were taken before the closing review's fixes, which added a few tests. The review measured a
full check from nothing again, at 118 s, while the load average rose from 16 to 89.

Four-fifths of a check with nothing changed is the fuzzing, 20 s per target; a check from nothing
also builds libFuzzer and the nightly's sanitizer builds. The engine has no program yet, so the 2 s
from a change to the engine relaunched and ready is not measured.
