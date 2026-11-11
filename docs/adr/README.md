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
| [0007](0007-the-audio-engine.md) | The audio engine | Accepted; amended by 0018 |
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
