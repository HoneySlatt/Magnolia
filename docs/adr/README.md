# Architecture decision records

An ADR records one decision: its context, the decision, the alternatives rejected and the
consequences. Once accepted, only its status changes. A later decision that replaces it whole makes
it `Superseded by NNNN`; one that changes part of it makes it `Accepted; amended by NNNN`, and the
newer one `Accepted; amends NNNN (what)`.

New ADRs copy [`0000-template.md`](0000-template.md) and take the next free number.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-what-magnolia-is.md) | What Magnolia is | Accepted; amended by 0017 |
| [0002](0002-gpl-3-0-or-later-copyright-kept.md) | GPL-3.0-or-later, its copyright kept by the owner | Accepted; amended by 0012 |
| [0003](0003-every-plugin-out-of-the-engines-process.md) | Every plugin out of the engine's process | Accepted |
| [0004](0004-magnolias-own-wine-bridge-on-the-systems-wine.md) | Magnolia's own Wine bridge, on the system's Wine | Accepted |
| [0005](0005-plugin-guis-floating-first-then-inside-the-window.md) | Plugin GUIs floating first, then inside the window | Accepted; amended by 0015 |
| [0006](0006-the-interface-on-uwui-after-the-engine-and-the-project.md) | The interface on uwui, after the engine and the project | Accepted; amended by 0011 |
| [0007](0007-the-audio-engine.md) | The audio engine | Accepted; amended by 0018, 0023, 0024, 0025, 0026 and 0032 |
| [0008](0008-the-project-model.md) | The project model: tracks with channels, linked patterns | Accepted |
| [0009](0009-the-project-in-sqlite-nothing-lost.md) | The project in SQLite; nothing lost | Accepted |
| [0010](0010-the-quality-bar.md) | The quality bar | Accepted; amended by 0011 and 0014 |
| [0011](0011-magnolia-no-longer-a-program-of-uwuis-first-stage.md) | Magnolia no longer a program of uwui's first stage | Accepted |
| [0012](0012-the-logo-its-rights-kept.md) | The logo, its rights kept | Accepted |
| [0013](0013-a-pinned-stable-toolchain-and-a-nightly-for-checks.md) | A pinned stable toolchain, and a nightly for checks only | Accepted; amended by 0019 |
| [0014](0014-the-quality-bar-measured-against-the-market.md) | The quality bar measured against the market | Accepted; amends 0010 |
| [0015](0015-the-interface-in-a-process-of-its-own.md) | The interface in a process of its own | Accepted; amends 0005 |
| [0016](0016-agents-on-the-shape-of-uwuis-agent-core.md) | Agents on the shape of uwui's agent core | Accepted |
| [0017](0017-macos-a-candidate-stage-after-stage-4.md) | macOS, a candidate stage after Stage 4 | Accepted; amends 0001 |
| [0018](0018-the-time-model.md) | The time model | Accepted; amends 0007 |
| [0019](0019-the-sanitizers-nightly-code-behind-a-cfg.md) | The sanitizer's nightly code behind a cfg | Accepted; amends 0013 |
| [0020](0020-clippy-forbids-the-locks-the-sanitizer-cannot-see.md) | Clippy forbids the locks the sanitizer cannot see | Accepted |
| [0021](0021-the-node-contract.md) | The node contract | Accepted; amended by 0025 and 0027 |
| [0022](0022-a-panic-on-the-real-time-path-gives-silence.md) | A panic on the real-time path gives silence, and is reported | Accepted; amended by 0032 and 0034 |
| [0023](0023-the-real-time-allowlist.md) | The real-time allowlist | Accepted; amends 0007; amended by 0026 and 0032 |
| [0024](0024-how-a-swap-avoids-a-click.md) | How a swap avoids a click | Accepted; amends 0007 |
| [0025](0025-delay-compensation-by-reading-ahead.md) | Delay compensation by reading ahead | Accepted; amends 0007 and 0021; amended by 0032 |
| [0026](0026-one-stream-per-audio-track.md) | One stream per audio track, from the disk reader | Accepted; amends 0007 and 0023 |
| [0027](0027-timeline-events-from-sources-on-nodes.md) | Timeline events from sources attached to nodes | Accepted; amends 0021 |
| [0028](0028-the-test-session-in-json.md) | The test session in JSON | Accepted |
| [0029](0029-magnolias-own-wav-and-midi-file-readers.md) | Magnolia's own WAV and MIDI file readers | Accepted; amended by 0030 |
| [0030](0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md) | Takes in WAV files of unknown length, repaired on recovery | Accepted; amends 0029; amended by 0033 |
| [0031](0031-the-round-trip-reported-and-takes-placed-by-it.md) | The round trip reported, and takes placed by it | Accepted |
| [0032](0032-recording-by-a-capture-tap-of-the-processor.md) | Recording by a capture tap of the processor | Accepted; amends 0007, 0022, 0023 and 0025; amended by 0034 |
| [0033](0033-takes-in-24-bit-integers-as-an-option.md) | Takes in 24-bit integers, as an option | Accepted; amends 0030 |
| [0034](0034-no-play-after-the-graphs-panic.md) | After the graph's panic, stops are kept and plays refused | Accepted; amends 0022 and 0032 |
