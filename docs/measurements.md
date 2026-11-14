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
| The CI job | 2 min 53 s | 20 min |

The CI job's time is that of run 43022262682, of commit `c517087`, one run; the JACK backend's tests
ran there on JACK2's dummy driver. The 2 s budget asks for "a session without plugins": until M1.4 plays sessions, the example's sine
stands in for one. A check with nothing changed is mostly the fuzzing (40 s), the JACK backend's
tests, which play in real time on their dummy servers, and the release build of the page-fault test.

## M1.4 — A session plays

Taken on 2026-11-11 on the high tier (AMD Ryzen 7 5800X, 8 cores, 16 threads, NVMe), with rustc
1.99.1, the nightly of 2026-11-05 and JACK2 1.9.22, inside the development shell. Other work ran on
the machine: the load average is noted with each table.

### The callback on the reference projects, without plugins

`magnolia play` on each generated reference project ([reference projects](reference-projects.md#generating-them))
for 120 s, on a JACK2 server of its own, `jackd -r -d dummy -r 48000`: the dummy driver, without
real time. Release build. The instrument tracks play `TestSynth`; buses and sends are gains. One run
each; quantiles are the upper bounds of the histogram's buckets, within 6.25 %.

| Project | Period | Periods | Median | 99.9th percentile | Maximum | Disk underruns | Load |
|---|---|---|---|---|---|---|---|
| High tier: 48 audio tracks, 24 MIDI tracks | 64 frames (1.33 ms) | 90,087 | 49.2 µs | 111 µs | 538 µs | 0 | 3.0 |
| Lowest tier: 16 audio tracks, 8 MIDI tracks | 128 frames (2.67 ms) | 45,043 | 34.8 µs | 111 µs | 422 µs | 0 | 1.0–2.1 |

The worst callbacks are 40 % and 16 % of their periods, the 99.9th percentiles 8.3 % and 4.1 %.
These are not the [floor's](ROADMAP.md#floors) figures: the server had no real-time priority, and
recorded 8 and 69 xruns and 4 and 30 gaps; on a dummy server without real time the backend's own
overruns (23 and 25) count callbacks the server started late, not long ones. The floors are measured
by the owner on the hardware ([PLAN](PLAN.md#m14--a-session-plays)).

### The disk reader's read-ahead

`cargo bench --bench disk_reader`: 16 tracks of 24-bit stereo, 30 s each, a read-ahead of 10 s.
"Cold" drops the files from the page cache first (`dd iflag=nocache`). Load 1.7 to 1.1.

| Run | First fill | Refills | Throughput |
|---|---|---|---|
| Swept without a thread, cold | 35.9 ms for 43.3 MB | 2.48 ms on average, 5.53 ms the longest | 1,744 MB/s |
| Swept without a thread, warm | — | 2.33 ms on average | 1,850 MB/s |
| Real time, on its thread, cold | primed in 9.5 ms | 5 of 19 MB, 11.5 ms each | 1,657 MB/s |
| Real time, on its thread, warm | primed in 3.2 ms | 11.0 ms each | 1,719 MB/s |

From a player's request to the reader's publication: 229 µs at the median, 236 µs the longest,
cold; 238 and 246 µs warm. Over 30 s, every player's low-water mark was 239,872 frames, its refill
threshold less one period: each refill landed before the player's next period. On this NVMe the
reader is bound by its own work, not the disk: cold and warm are close.

Through the reference projects above, the lowest low-water marks were 44,992 frames (0.94 s, high
tier) and 88,356 frames (1.84 s, lowest tier), both while the play began: a play starts once half
a second of each stream is ready, then the reader fills the rest of its 10 s while it plays. The
reader read 930 MB in 33 refills, the longest 70 ms (high tier), and 251 MB, the longest 21 ms
(lowest tier). The lowest tier's own storage is measured by the owner.

Decoding a block of 4,096 stereo frames, warm: Int24 3.4 µs (0.84 ns a frame), Int16 0.77 µs, Int32
0.54 µs, Float32 0.52 µs.

### Generating and rendering the reference projects

`cargo run --release -p magnolia-session --example generate`, and the offline render, in blocks of
4,096 frames, of the 211 s each plays.

| Project | Files | Generated in | Rendered in |
|---|---|---|---|
| Lowest tier | 665 MB | 3.7 s | 1.79 s, 118 times real time |
| High tier | 2.18 GB | 12.5 s | 4.68 s, 45 times real time |

Both renders had 0 underruns. A session's memory budget estimates 43.9 MiB (lowest tier) and
141.9 MiB (high tier).

### Compiling and checking

The median of three runs unless noted, in a target folder of its own. Load average 1.7 to 3.2 for
the builds, 1.7 to 7.2 for the checks.

| What | Time | Budget ([floors](ROADMAP.md#floors)) |
|---|---|---|
| A debug build of the workspace from nothing | 4.2 s | 3 min |
| A release build from nothing | 5.2 s | 10 min |
| A debug build after a change to the engine, a low-level crate | 0.58 s | 10 s |
| A change to the engine, `magnolia play` rebuilt, relaunched and playing the `small` session on a dummy JACK2 | 1.1 s (the build 1.0 s) | 2 s |
| A full check from nothing | 173 s (147, 190, 173) | 3 min |
| A full check, nothing changed, one run | 85 s | — |
| The CI job | 5 min 25 s | 20 min |

The full check was taken again after the closing review's fixes, which added a fuzz target: the
load rose from 2.3 to 15 during the three runs, as other work started; the first, from a quiet
machine, took 147 s. The median meets the budget by 7 s only: the next milestone's tests will pass
it unless the tests' step, now about a minute and a half, gets shorter. The CI job's time is that of
run 43099839265, of commit `1efc4bd`, one run. Before the fuzz targets
ran side by side, the full check took 298 s from nothing and 232 s with
nothing changed, over its budget: five targets of 20 s one after the other. `scripts/check.sh` now
prints each step's time; the tests take about a minute, the JACK backend's playing in real time on
their dummy servers.

## M1.5 — Recording and monitoring

Taken on 2026-11-14 on the high tier (AMD Ryzen 7 5800X, 8 cores, 16 threads, NVMe, btrfs), with
rustc 1.99.1, the nightly of 2026-11-05 and JACK2 1.9.22, inside the development shell. The round
trip through the Focusrite is the owner's to measure ([PLAN](PLAN.md#m15--recording-and-monitoring)).

### The recorder

`cargo bench --bench recorder`: mono noise recorded for 10 s in real time, in periods of 256 frames,
on the bench's thread at normal priority; the writer syncs each take's data every second of its
audio, on a pool of sync threads. Load average 3.0 to 1.2. No frame was dropped in any run, and every
ring's fullest fill was its 2,048-frame wake threshold: the writer always kept up.

| Armed tracks | Sync threads | Writer's pass, mean | Longest pass | Syncs | Longest sync | Callback: median, 99.9th |
|---|---|---|---|---|---|---|
| 1 | 4 | 52 µs | 89 µs | 13 | 1.8 ms | 12 µs, 30 µs |
| 8 | 4 | 166 µs | 291 µs | 97 | 2.3 ms | 8.7 µs, 55 µs |
| 32 | 8 | 557 µs | 11.7 ms | 385 | 7.9 ms | 18 µs, 238 µs |
| 32 | 4 (the default) | 501 µs | 1.13 ms | 369 | 8.0 ms | 16 µs, 74 µs |
| 32 | 2 | 513 µs | 1.09 ms | 360 | 4.7 ms | 16 µs, 86 µs |
| 32 | 1 | 514 µs | 0.94 ms | 359 | 6.9 ms | 18 µs, 94 µs |

Eight `fdatasync` calls at once slow one another on this btrfs and stall the writer's writes: 5.5 to
57 ms in five earlier runs, against about 1.1 ms with four threads, which keep the syncs parallel.
The callback's figures include the capture tap and a test graph, not the reference projects. The
lowest tier's storage is measured by the owner.

### The tests that prove it

- Killed mid-take with SIGKILL after at least 200,000 frames, a take is recovered whole: every frame
  the recorder had written, exact, its `bext` time reference right (`tests/engine/record.rs`).
- A simulated power cut, 300 seeds, keeps every frame synced before it, exact, and whole frames only
  (`record/tests.rs`).
- Takes are placed to the sample for round trips of 0 to 5,000 frames, periods of 1 to 4,096 frames,
  a play while rolling and a swap during the take.
- On JACK2's dummy driver, an impulse through Magnolia's monitoring path arrives on the same sample
  as a direct one, and 100 samples later through a `TestLatency` of 100 (`tests/jack/round_trip.rs`);
  the dummy reports one period of capture latency and two of playback, at 64 and 256 frames.

### Compiling and checking

The median of three runs, in a target folder of its own. Load average 2.5 to 4.6.

| What | Time | Budget ([floors](ROADMAP.md#floors)) |
|---|---|---|
| A debug build of the workspace from nothing | 4.6 s (4.7, 4.6, 4.6) | 3 min |
| A release build from nothing | 5.7 s (5.8, 5.7, 5.7) | 10 min |
| A debug build after a change to the engine, a low-level crate | 0.70 s | 10 s |
| A change to the engine, `magnolia` rebuilt, relaunched and playing the `small` session on a dummy JACK2 for 0.1 s, then stopped | 1.05 s (the build 0.70 s) | 2 s |
| A full check from nothing | 153 s (153, 144, 194) | 3 min |
| A full check, nothing changed, one run | 83 s | — |
| The CI job | 4 min 53 s | 20 min |

The load rose from 0.8 to 18.8 during the three full checks, as other work started; the third, at
the highest load, took 194 s, over the budget. Another run from nothing, earlier that day at a load
of 3.5, took 144 s. The CI job's time is that of run 43568954353, one run. The tests' step went from
about 90 s at M1.4 to about 45 s: the JACK backend's tests now run in
JACK2's freewheel mode, 10 to 17 s against 38 to 64 s before, though M1.5 added the recorder's, the
latency's, the round trip's and the command line's tests.
