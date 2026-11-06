# Roadmap

Magnolia's direction: what it is for, its quality bar, its decisions, and its stages. The current
work is in [`PLAN.md`](PLAN.md); each decision is an ADR in [`adr/`](adr/README.md). Stages are
merged, reordered or dropped as the project learns; each such change is an ADR.

## 1. Vision

A DAW of the class of FL Studio and Pro Tools, on Linux first, then Windows
([ADR-0001](adr/0001-what-magnolia-is.md)). The composer's speed of FL Studio, its piano roll,
patterns and step sequencer, with the recording, editing and mixing of Pro Tools, in one program.
It hosts every VST3 and CLAP plugin, and Windows plugins on Linux through a bridge of its own, and
survives any plugin's crash. It serves film and game music: video, timecode, loops and stems. Its
interface, on uwui, is dark, dense, refined, accessible, drawn at 240 Hz and still at rest.

## 2. Principles

- The sound comes first: nothing the interface, the disk or a plugin does may cause an xrun.
- Nothing is ever lost ([ADR-0009](adr/0009-the-project-in-sqlite-nothing-lost.md)).
- Everything is measured; a budget missed is debt until it is met.
- Every edit is a command: undo, tests, agents and screen readers use the same path.
- Every feature works on the lowest tier; only the number of tracks and visual detail drop.

## Quality bar

Accepted by the owner on 2026-11-06 ([ADR-0010](adr/0010-the-quality-bar.md)). Figures measured go
to `docs/measurements.md`, created with the first measure. Nothing is measured yet.

### References

| Area | Reference | What it is best at |
|---|---|---|
| Composition | FL Studio | Piano roll (ghost notes, stamps, scales), patterns and playlist, step sequencer, speed |
| Audio editing and mixing | Pro Tools | Editing by the keyboard, playlists and comping, mixing, large sessions, delay compensation |
| Engine efficiency and start | REAPER | Render-ahead on worker threads, a small footprint, a start of seconds |
| Plugin isolation | Bitwig Studio | Plugins in processes apart, a crash recovered with a reload |
| Arrangement and routing | Studio One (Fender Studio Pro) | Arranger track, drag and drop, child channels of multi-output instruments |
| Warping | Ableton Live | Audio stretched to tempo |
| Linux and routing | Ardour | Any routing, sessions on Linux |
| Accessibility | REAPER with OSARA | Every state, position and parameter spoken, the whole program by the keyboard |
| Round-trip latency | RME Babyface Pro FS | About 4 ms at 64 frames on Windows, the lowest found for USB hardware |

### Hardware

- **High tier**: the owner's desktop, RX 6900 XT class, 8 cores, 32 GB, a display at 3840 × 2160 and
  240 Hz: 4.17 ms a frame. A Focusrite Scarlett or Clarett interface (its model recorded with the
  first measure).
- **Lowest tier**: the owner's HP Laptop 17-ca2xxx, AMD Athlon Silver 3050U (2 cores), integrated
  Vega, 4 GB on one channel. Its display, storage and Vulkan driver are not yet known; measured
  first, before anything is bought.
- Windows tiers are named at the Windows stage. macOS is not measured.

### Budgets

A *period* is the block of frames that the audio device asks for at each callback. A *reference
project* is fixed in M1.3, one for each tier, and never tuned for afterwards.

| Budget | Figure | Measured by | Checked in |
|---|---|---|---|
| Period, high tier | 64 frames at 48 kHz on the high tier's reference project, the owner's Wine plugins included: no xrun for one hour | the backend's xrun count, `pw-top` | by hand at each engine milestone |
| Callback time, high tier | the worst callback under 85 % of the period; the 99.9th percentile under 50 %; wall time, waits on plugin hosts included | the engine's own histogram of callback times | by hand at each engine milestone |
| Period and callback, lowest tier | 128 frames at 48 kHz on the lowest tier's reference project, same rules | as above | by hand |
| Engine efficiency | the processor's time per period on the reference project no higher than REAPER's on the same project, generated as an `.rpp` file | both programs' processor time, plugins alike | by hand at Stage 1's end |
| Latency added | none: the round trip through Magnolia's monitoring path, plugin hosts in it, equals a direct loopback through the JACK API, to the sample | a loopback cable and `jack_iodelay`, each reopened several times | by hand |
| The real-time path | no allocation, no lock, no system call outside the allowlist (futex, eventfd) | RealtimeSanitizer, the allowed calls suppressed by name | CI |
| A plugin's round trip | at least as good as yabridge's on the same plugin, per period and per plugin host; its figure set by Stage 1's prototype | the bridge's benchmark | CI on test plugins; by hand under Wine with the owner's |
| A plugin late or dead | a silent block, never an xrun; the deadline set in Stage 1 | a test plugin that hangs and one that crashes | CI |
| MIDI input | timestamp jitter under 0.5 ms | a cable from the interface's MIDI output to its input, notes sent by a second program | by hand |
| Offline render | nulls against real-time playback for Magnolia's own DSP, to the last bit; faster than real time by a factor set in Stage 2 | a render of the reference project | CI |
| Interface, high tier | at 240 Hz and 4K, the 99th percentile under 4.17 ms and no frame over 8.33 ms, while playing, scrolling and zooming the [screen of reference](uwui-needs.md#a-screen-of-reference-for-magnolia) | uwui's frame trace and `scripts/measure.py` | by hand at each interface milestone |
| Interface, lowest tier | at the display's rate; at 60 Hz, the 99th percentile under 16.7 ms and no frame over 33.3 ms, on a smaller screen fixed at Stage 3's start; the audio's callback budget still met | as above, the audio's histogram beside it | by hand |
| In step with the sound | meters and playhead within one frame of the sound heard | the presentation time of frames against the engine's clock and the latency the backend reports; checked once physically, by hand, at Stage 3 | by hand |
| Input to present | one frame at the 99th percentile (uwui's budget) | uwui's trace | by hand |
| At rest | as the [glossary](glossary.md) defines rest: no frame drawn; no wake-up of Magnolia's own outside the device's callback; plugin hosts receive no calls | context switches over 10 s, `pidstat -w` | CI for the interface; by hand for the engine |
| Power at rest and playing | measured, the GPU's power and the processor's time, plugin GUIs closed and open; its figure set at Stage 3 | the GPU's power reading, `top` | by hand |
| Start, Stages 1 and 2 | the engine ready to play an empty session within 100 ms warm on the high tier | `hyperfine` on the command line's start | by hand |
| Start, from Stage 3 | warm to the first useful frame within 100 ms, cold within 500 ms, on the high tier; then "playable" (plugins still loading shown as silent slots) and "fully loaded" measured on the reference projects, on both tiers | the log's times | by hand |
| Plugin scan | never blocks playing or the interface; its time for the owner's plugins measured | the log's times | by hand |
| Memory | an empty project at rest within 100 MB on the high tier; on the lowest tier, everything within 2.5 GB on its reference project, plugin hosts and the private Xwayland counted | the proportional size of all of Magnolia's processes | by hand |
| Disk streaming | the number of tracks of audio the lowest tier streams without xrun, measured | the reference project, tracks added | by hand |
| Saving | a gesture's commit off the interface's thread; its sync's 99th percentile and the window of gestures a power cut may lose set in Stage 2 on the lowest tier's storage; no gesture whose sync returned is lost | a SQLite file layer that drops unsynced writes | CI |
| Bad input | project, MIDI, audio and plugin-state files never crash Magnolia | `cargo fuzz` | CI, briefly; longer by hand |
| `unsafe` code | sound under Miri where Miri can run it (shared memory and system calls cannot) | Miri | CI |
| Iteration | a CI job under 20 minutes; a full `scripts/check.sh` timed at M1.1, its budget set then | the job's time | CI |

A crash leaves a report on the machine; nothing leaves it without the user's consent.

### Without a figure

- WCAG 2.2 at level AA through uwui; every control drawn by Magnolia described for screen readers;
  a generic editor of every plugin's parameters; levels read aloud.
- Reduced motion and reduced transparency honored; effects lighter on the lowest tier.
- The interface compared with reference images in CI, at scales 1 and 2, from Stage 3.
- HDR and wide gamut: not supported; variable refresh rates: as uwui supports them.
- Text ready for translation.
- Only AMD GPUs are measured; NVIDIA and Intel are stated as unmeasured.

## 3. Decisions

### Fixed

| Topic | Decision |
|---|---|
| What Magnolia is | A DAW of FL Studio's and Pro Tools' class, for film, games and songs ([ADR-0001](adr/0001-what-magnolia-is.md)) |
| License | GPL-3.0-or-later, all the copyright kept by the owner ([ADR-0002](adr/0002-gpl-3-0-or-later-copyright-kept.md)) |
| Plugins | Every plugin out of the engine's process, the unit measured ([ADR-0003](adr/0003-every-plugin-out-of-the-engines-process.md)) |
| Windows plugins | Magnolia's own bridge, on the system's Wine ([ADR-0004](adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)) |
| Plugin GUIs | Floating first, then inside the window ([ADR-0005](adr/0005-plugin-guis-floating-first-then-inside-the-window.md)) |
| Interface and order | On uwui, after the engine and the project ([ADR-0006](adr/0006-the-interface-on-uwui-after-the-engine-and-the-project.md)) |
| Engine | Own duplex backends, a compiled graph, exact time ([ADR-0007](adr/0007-the-audio-engine.md)) |
| Project model | Tracks with channels, linked patterns ([ADR-0008](adr/0008-the-project-model.md)) |
| Project file | SQLite, nothing lost ([ADR-0009](adr/0009-the-project-in-sqlite-nothing-lost.md)) |
| Quality bar | Above ([ADR-0010](adr/0010-the-quality-bar.md)) |
| Platforms | Linux first, Windows at a later stage, macOS within reach without a port |
| Language | Rust, stable |

### Open decisions

Each is decided by an ADR when its stage needs it, not earlier.

| Topic | Candidates | Decided at |
|---|---|---|
| The unit of plugin isolation, and a late plugin's deadline | one process per plugin; one per track's chain; per tier | Stage 1, on the prototype's figures |
| How often a plugin's state is read when it never reports a change | an interval, against its cost on the host's main thread | Stage 1 |
| The audio device at rest | kept open; released after a while at rest, reopened on play or arming | Stage 1, on the measured cost of each |
| The figures left to measure | a plugin's round trip, the render's factor, the saving's sync and its window, power, the check's time | each at the milestone that measures it |
| How the Windows host talks to the engine | a pure Windows executable; a winelib program | Stage 1, on the prototype's figures |
| Render-ahead of tracks without live input | REAPER's way; the graph scheduler alone | Stage 2, on measures of heavy projects |
| Time-stretch | signalsmith-stretch (MIT), Bungee (MPL-2.0), Rubber Band (GPL-2.0-or-later) | Stage 4 |
| Video decoding | uwui's video crate, FFmpeg for ProRes and DNxHD | the film stage |
| Exchange with film editors (AAF) | none; AAF export | the film stage |
| Wwise and FMOD exports | files and metadata each tool imports | the film and games stage |
| Stem separation | its model and its weights' license | its stage |
| ARA under Wine | research; Windows only | after ARA on Windows |
| The order of the stages after Stage 4 | film and games, the sampler and drum machine, plugin GUIs inside the window, Windows, stems and ARA | the end of Stage 4 |
| Contribution policy | a CLA; one copyright holder, stated | before outside contributions open |
| Packages for Linux | Nix, AppImage, Flatpak (whose sandbox fights plugin paths and Wine) | before the first binary |

### What is native, what is optional

Native means in every build, held to the bar. Optional means a feature the user may leave out, or
a component that installs apart.

| Need | Native or optional | Why, for the long term |
|---|---|---|
| VST3 and CLAP hosting, isolation | Native | The reason for the program |
| The Wine bridge | Native on Linux; it needs the system's Wine | The owner's plugins are Windows plugins |
| ARA (Melodyne) | Native on Windows; research under Wine | Vocal editing without round trips |
| Piano roll, patterns, step sequencer, comping, mixer, automation | Native | The two halves of the owner's work |
| Mixing essentials, loudness meter | Native | Mixing without installing anything |
| Simple sampler; then sampler and drum machine | Native | The step sequencer needs sounds |
| Video track, timecode, sync points | Native | Film is half the owner's work |
| Game exports (loops, stems, Wwise, FMOD) | Native, as exports | Small, and needed for every game |
| Mastering document | Native | Albums and soundtracks |
| Stem separation | Built in, its model installed on demand | Its model weighs hundreds of megabytes; not every user wants it |
| Plugin GUIs inside the window | Native, floating windows as fallback | The owner's choice; a first on Linux |
| Accessibility | Native | Costly to add later |
| Command line and MCP server | Native | Tests, agents and scripting |
| Windows | A platform of its own stage | Linux first |

## 4. Stages

### Stage 1 — The engine and the plugins → 0.1

**Goal.** Magnolia plays, records and monitors at low latency on Linux, hosting the owner's
plugins, native and Windows, each out of the engine's process, without an interface.

- The time model; the JACK API backend, duplex; the compiled graph, delay compensation,
  sample-accurate events.
- Recording and input monitoring at low latency; recordings that survive a crash.
- Plugin-host processes for CLAP and VST3; the Wine bridge; the isolation's unit and the deadline
  measured and decided; crash, hang and reload.
- Plugin GUIs in floating windows; each plugin's parameters listed.
- A command line and an MCP server that drive the engine on a test session.
- The budgets of the sound and of rest measured on the reference projects.

**Exit.** The owner's plugins (Serum, FabFilter, Valhalla, Kontakt with a large library, Auto-Tune
in low-latency mode) play and record in a test session at 64 frames on the high tier for an hour
without xrun, the worst block under 85 % of the period, and each survives the kill of its process;
the figures are in `docs/measurements.md`.

### Stage 2 — The project → 0.2

**Goal.** A whole project, created, edited, saved and recovered without an interface.

- The model of [ADR-0008](adr/0008-the-project-model.md): tracks and channels, multi-output and
  multi-timbral instruments, routing, linked patterns, take lanes, automation, the tempo map.
- The SQLite file, its migrations, every edit a command, undo kept, snapshots.
- Recovery after simulated power cuts.
- Offline render, stems, loops with their points, MIDI files in and out.

**Exit.** A project of the reference's size is built by commands, survives a simulated power cut at
any gesture with nothing lost, and renders offline to stems that null against real-time playback.

### Stage 3 — The interface → 0.3

**Goal.** The owner makes a beat from nothing to an exported file in Magnolia, on uwui.

- The arrangement and the pattern lane, the piano roll, the step sequencer with a simple sampler,
  the mixer, the browser, the transport.
- Meters and playhead in step with the sound; docked panels on several monitors.
- Accessibility of every control.

**Exit.** The owner makes a beat in Magnolia from nothing to its export, and approves it; the
screen of reference meets its budgets on the high tier, and the lowest tier's smaller screen meets
its own.

### Stage 4 — Recording and audio editing → 0.4

**Goal.** A whole song, vocals included, recorded, edited and mixed in Magnolia: the owner no longer
needs Pro Tools for it.

- Comping on take lanes, fades, clip gain, editing by the keyboard, time-stretch.
- The built-in mixing essentials and the loudness meter.

**Exit.** The owner records, edits and mixes a song with vocals in Magnolia, and approves it.

### Later, their order decided at the end of Stage 4

- Film and games: the video track, timecode, sync points, game exports, the mastering document.
- The sampler and the drum machine.
- Plugin GUIs inside the window.
- Windows.
- Stem separation; ARA.
- 1.0: the whole of [ADR-0001](adr/0001-what-magnolia-is.md), audited.

## 5. Release map

| Version | Stage | Content |
|---|---|---|
| 0.1 | 1 | The engine and the plugins, without an interface |
| 0.2 | 2 | The project, without an interface |
| 0.3 | 3 | The interface |
| 0.4 | 4 | Recording and audio editing |
| 1.0 | Later | Everything ADR-0001 asks |
