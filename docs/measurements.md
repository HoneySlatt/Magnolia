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
gave about half of them, but most of its positions fell past the last tempo change. This version
was measured again on a quiet machine, below the table.

| What | 1 tempo change | 1,000 tempo changes |
|---|---|---|
| A tick placed on its sample | 26.6 ns | 49.2 ns |
| A sample read as a tick | 18.7 ns | 34.7 ns |
| A block: its ticks, then its 16 events placed | 403 ns | 906 ns |

Taken again on a quiet machine on 2026-11-11, during M1.3, with the same benchmark: load average 2
to 3, the median of three runs.

| What | 1 tempo change | 1,000 tempo changes |
|---|---|---|
| A tick placed on its sample | 11.3 ns | 25.1 ns |
| A sample read as a tick | 9.8 ns | 17.7 ns |
| A block: its ticks, then its 16 events placed | 191 ns | 494 ns |

A block of 64 frames at 48 kHz lasts 1.33 ms; its conversions take under 0.04 % of it, and under
0.07 % even under the load above. The cost grows with the number of tempo changes through the binary search alone. No
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
| The CI job | 2 min 17 s | 20 min |

The CI job's time is that of run 42953280799, of commit `e3a3a7d`, one run. The other figures were
taken before the closing review's fixes, which added a few tests. The review measured a
full check from nothing again, at 118 s, while the load average rose from 16 to 89.

Four-fifths of a check with nothing changed is the fuzzing, 20 s per target; a check from nothing
also builds libFuzzer and the nightly's sanitizer builds. The engine has no program yet, so the 2 s
from a change to the engine relaunched and ready is not measured.

## M1.3 — The JACK backend and the graph

Taken on 2026-11-11 on the high tier (AMD Ryzen 7 5800X, 8 cores, 16 threads), with rustc 1.99.1,
the nightly rustc 1.101.0 of 2026-11-05 and JACK2 1.9.22, inside the development shell. The owner's
own programs ran meanwhile: load average 6 to 17, noted with each table.

### The callback on a dummy JACK2 server

`examples/play.rs` with `--seconds 12 --swap-every 0 --load 64` (a sine through a gain on two
outputs, and 64 more chains of a sine and a gain), on `jackd -r -d dummy -r 48000`: JACK2's dummy
driver, which needs no sound card, run without real time. Release build. One run each; the
histogram's quantiles are the upper bounds of their buckets, within 6.25 %.

| Period | Periods | Median | 99.9th percentile | Maximum | Over 50 % / 85 % of the period |
|---|---|---|---|---|---|
| 256 frames (5.33 ms) | 2,250 | 213 µs | 351 µs | 351 µs | 0 % / 0 % |
| 64 frames (1.33 ms) | 9,001 | 55.3 µs | 98.3 µs | 310 µs | 0 % / 0 % |

Own overruns, gaps, server xruns, node errors and non-finite samples were all 0 in both runs. These
are not the [floor's](ROADMAP.md#floors) figures: the server had no real-time priority and the
machine was shared. The floor is measured by the owner, through `pw-jack` on the Focusrite, on the
high tier (see the [PLAN](PLAN.md#m13--the-jack-backend-and-the-graph)).

### At rest

The same program with nothing swapped, voluntary context switches of each of its threads read in
`/proc/<pid>/task/*/status` at 2 s and at 11 s.

| Thread | 256 frames | 64 frames |
|---|---|---|
| main, collector, JACK2's two client threads | +0 each | +0 each |
| JACK's process thread | +1,693 (one per period) | +6,765 (one per period) |

Nothing of Magnolia's wakes outside the device's callback. M1.3 has no transport yet, so "at rest"
here means no edit: the graph still plays its sine.

### No page fault in a swap

`page_faults::a_swap_takes_no_page_fault_on_the_processing_thread_after_warm_up`, the processing
thread's minor faults across each swap after the first round: 0 within the pool and 0 with a larger
pool, in debug and release builds. With the pool's pre-write removed by hand, once, the release build
took 49 to 428 faults per swap.

### Compiling and checking

`time`-like wall clock in the development shell, the median of three runs unless noted. Load
average 8 to 17.

| What | Time | Budget ([floors](ROADMAP.md#floors)) |
|---|---|---|
| A debug build of the workspace from nothing | 1.56 s | 3 min |
| A release build from nothing | 1.91 s | 10 min |
| A debug build after a change to the engine, a low-level crate | 0.44 s | 10 s |
| A change to the engine, the example rebuilt and playing on a dummy JACK2 | 0.77 s | 2 s |
| A full check from nothing, one run | 93.6 s | 3 min |
| A full check, nothing changed, one run | 74.7 s | — |

The 2 s budget asks for "a session without plugins": until M1.4 plays sessions, the example's sine
stands in for one. A check with nothing changed is mostly the fuzzing (40 s), the JACK backend's
tests, which play in real time on their dummy servers, and the release build of the page-fault test.
