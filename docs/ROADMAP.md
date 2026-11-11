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

Accepted by the owner on 2026-11-06 ([ADR-0010](adr/0010-the-quality-bar.md)) and measured against
the market since 2026-11-10 ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
Figures measured go to [`measurements.md`](measurements.md); only the check's times and the time model's conversions are measured yet.

### How the bar works

- A **floor** is an absolute figure. It gates the milestone that measures it.
- A **lead** is a margin over the best rival, measured. A lead missed is debt, never a gate. Its
  figure is set once the rival is measured. The rival's version is frozen until the next stage
  audit, which measures it again.
- Where the hardware sets the limit, as for added latency, the floor alone is the bar.
- **Rivals**: the engine is measured on Linux against REAPER, Bitwig Studio and Ardour. Creation and
  the interface are also measured against FL Studio, Pro Tools, Ableton Live, Studio One and
  Cubase, in the owner's Windows virtual machine. Those figures are indicative, never a bar for
  time, power or frames, until the Windows stage measures them on Windows installed on a disk of
  its own.
- Each rival's figure names its version, its settings and its date. A measure on the owner's
  licenses or accounts is taken by the owner.

### References

| Area | Reference | What it is best at |
|---|---|---|
| Composition | FL Studio | Piano roll (ghost notes, stamps, scales), patterns and playlist, step sequencer, speed |
| Audio editing and mixing | Pro Tools | Editing by the keyboard, playlists and comping, mixing, large sessions, delay compensation |
| Engine efficiency and start | REAPER | Render-ahead on worker threads, a small footprint, a start of seconds |
| Plugin isolation | Bitwig Studio | Plugins in processes apart, a crash recovered with a reload; its engine a process of its own |
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
  first, before anything is bought. It is measured on mains power, its frequency governor and audio
  device named.
- Windows tiers are named at the Windows stage. macOS is not measured
  ([ADR-0017](adr/0017-macos-a-candidate-stage-after-stage-4.md)).

### Floors

A *period* is the block of frames that the audio device asks for at each callback. A *reference
project* is fixed in M1.3, one for each tier, and never tuned for afterwards.

| Budget | Floor | Measured by | Checked in |
|---|---|---|---|
| Period, high tier | 64 frames at 48 kHz on the high tier's reference project, the owner's Wine plugins included: no xrun and no missed deadline for eight hours | the backend's xrun count, the engine's count of missed deadlines, `pw-top` | by hand at each engine milestone |
| Callback time, high tier | the worst callback under 85 % of the period; the 99.9th percentile under 50 %; wall time, waits on plugin hosts included | the engine's own histogram of callback times | by hand at each engine milestone |
| Period and callback, lowest tier | 128 frames at 48 kHz on the lowest tier's reference project, same rules | as above | by hand |
| The lowest tier's reference project | a song with vocals over a beat: 16 audio tracks streamed from disk, 8 instrument tracks, 16 plugins of which 4 Windows plugins under Wine; defined and its plugins named at M1.3, provisional until the laptop is measured; within the memory budget below, measured at M1.9 | the reference project | by hand at M1.3 and M1.9 |
| Engine efficiency | the processor's time per period on the reference project at most 1.1 times REAPER's on the same project, generated as an `.rpp` file | both programs' processor time, plugins alike | by hand at Stage 1's end |
| Processing power | at least 0.9 times what REAPER holds running the plugin inside REAPER's own process, its default, at 64 and 128 frames; the figure at 32 frames recorded; revised on M1.6's prototype | DAWbench's method: instances of one plugin, then an instrument's voices, added until the first xrun or missed deadline | by hand at M1.6 and M1.9 |
| Latency added | none: the round trip through Magnolia's monitoring path, plugin hosts in it, equals a direct loopback through the JACK API, to the sample | a loopback cable and `jack_iodelay`, each reopened several times | by hand |
| The real-time path | no allocation, no lock, no system call outside the allowlist (futex, eventfd) | RealtimeSanitizer, the allowed calls suppressed by name | CI |
| A plugin's round trip | at least as good as yabridge's on the same plugin, per period and per plugin host | the bridge's benchmark | CI on test plugins; by hand under Wine with the owner's |
| The Wine bridge's plugins | each plugin of the test set (the owner's Windows plugins, and the free Windows VST3 and CLAP plugins the owner installs) that works under yabridge loads, plays, opens its GUI, keeps its state and survives a kill; the list of results public | the test set | by hand at M1.7 and at each stage audit |
| A plugin late or dead | a silent block, never an xrun; the deadline set in Stage 1 | a test plugin that hangs and one that crashes | CI |
| A plugin's bad output | a NaN, an infinity or a runaway level from a plugin never reaches an output | a test plugin that emits each | CI |
| A plugin's reload | a plugin that crashed is reloaded with its last state within a time set at M1.6 | a test plugin that crashes | CI |
| Plugin conformance | each CLAP and VST3 feature the engine supports passes its test plugin: among them a sidechain, multiple outputs, note expressions, a change of latency, sample-accurate automation | the project's test plugins | CI |
| MIDI input | timestamp jitter under 0.5 ms | a cable from the interface's MIDI output to its input, notes sent by a second program | by hand |
| Offline render | nulls against real-time playback for Magnolia's own DSP, to the last bit; faster than real time by a factor set in Stage 2 | a render of the reference project | CI |
| Signal processing | resampling at least equal to the best of r8brain and SoX in alias rejection, passband ripple and noise; the loudness meter passes EBU Tech 3341's test signals; the limiter lets no true-peak over through; dither on export when the bit depth is reduced | test signals | CI, from the stage that builds each |
| Large sessions | 1,000 tracks and 50,000 clips opened, edited and played on the high tier; the time of an edit measured, its figure set at Stage 2 | a generated project | by hand at Stage 2's end |
| Interface, high tier | at 240 Hz and 4K, 99.9 % of frames within 4.17 ms and none over 6.25 ms, from Magnolia's own trace, while playing, scrolling and zooming the [screen of reference](uwui-needs.md#a-screen-of-reference-for-magnolia); the compositor's stalls counted apart | uwui's frame trace and `scripts/measure.py` | by hand at each interface milestone |
| Interface, lowest tier | at the display's rate; at 60 Hz, 99.9 % of frames within 16.7 ms and none over 25 ms, on a smaller screen fixed at Stage 3's start; the audio's callback budget still met | as above, the audio's histogram beside it | by hand |
| In step with the sound | meters and playhead within one frame of the sound heard, scan-out and the panel's response allowed for | the presentation time of frames against the engine's clock and the latency the backend reports; a light sensor on the screen recorded with the audio, from SlimeEngine's Stage 6e | by hand at Stage 3 |
| Input to present | one frame at the 99th percentile (uwui's budget), scan-out and the panel's response allowed for | uwui's trace; the light sensor | by hand |
| At rest | as the [glossary](glossary.md) defines rest: no frame drawn; no wake-up of Magnolia's own outside the device's callback; plugin hosts receive no calls | context switches over 10 s, `pidstat -w` | CI for the interface; by hand for the engine |
| Power at rest and playing | measured, the GPU's power and the processor's time, plugin GUIs closed and open; its figure set at Stage 3 | the GPU's power reading, `top` | by hand |
| Start, Stages 1 and 2 | the engine ready to play an empty session within 100 ms warm on the high tier | `hyperfine` on the command line's start | by hand |
| Start, from Stage 3 | warm to the first useful frame within 100 ms, cold within 500 ms, on the high tier; then "playable" and "fully loaded" measured on the reference projects, on both tiers | the log's times | by hand |
| Plugin scan | never blocks playing or the interface; its time for the owner's plugins measured | the log's times | by hand |
| Memory | an empty project at rest within 100 MB on the high tier; on the lowest tier, everything within 2.5 GB on its reference project, plugin hosts and the private Xwayland counted; each plugin host's memory measured apart, its budget set at M1.6 | the proportional size of all of Magnolia's processes | by hand |
| Disk streaming | the number of tracks of audio the lowest tier streams without xrun, measured | the reference project, tracks added | by hand |
| Saving | a gesture's commit off the interface's thread; its sync's 99th percentile and the window of gestures a power cut may lose set in Stage 2 on the lowest tier's storage; no gesture whose sync returned is lost | a SQLite file layer that drops unsynced writes | CI |
| Crashes | no xrun caused by Magnolia during a take, an xrun being Magnolia's when its own callback overran; the engine and the interface killed at random lose nothing: every gesture the engine acknowledged, every sample written before the kill; from Stage 3, a crash of the interface leaves the sound and the take going ([ADR-0015](adr/0015-the-interface-in-a-process-of-its-own.md)) | the backend's xrun log against the engine's histogram of callback times during takes; a test that kills processes at random | CI for the kills; by hand for takes |
| Bad input | project, MIDI, audio and plugin-state files never crash Magnolia | `cargo fuzz` | CI, briefly; longer by hand |
| `unsafe` code | sound under Miri where Miri can run it (shared memory and system calls cannot) | Miri | CI |
| Compiling | on the high tier, in a debug build, 2 s from a change to the engine relaunched and ready on a session without plugins; 10 s after a change to a low-level crate, for the Linux target, the Windows plugin host measured apart; 3 min from nothing, set again when uwui joins; 10 min for the release build; from Stage 3, a change to the interface seen within 2 s | `time` on the builds | by hand at each milestone |
| Iteration | a CI job under 20 minutes; a full `scripts/check.sh` from a clean build within 3 minutes on the high tier | the job's time; `time` on the script | CI; by hand at each milestone |
| Accessibility | a checklist drawn from OSARA: every state, position, parameter and level spoken, the whole program by the keyboard, with Orca on Linux and NVDA on Windows | AT-SPI; NVDA by hand | CI from Stage 3 for Orca; by hand from the Windows stage for NVDA |

A crash leaves a report on the machine; nothing leaves it without the user's consent.

### Leads

Each lead's figure is set once its rivals are measured, and missed leads are debt.

| Family | Lead | Rivals | Measured at |
|---|---|---|---|
| Latency | 32 frames on the high tier, 64 on the lowest (provisional until M1.3 measures the laptop), under the floor's rules; on a reference project with a voice monitored live, the smallest period held never larger than the best rival's | REAPER, Bitwig Studio, Ardour | M1.9, then each stage audit |
| Engine efficiency | the processor's time per period below REAPER's on the same project | REAPER | Stage 1's end |
| Processing power | 1.25 times what the best rival that isolates plugins likewise holds, at 32, 64 and 128 frames | Bitwig Studio's isolated modes, REAPER's dedicated process | M1.6, M1.9 |
| The lowest tier | 1.25 times the tracks and plugins REAPER holds on the laptop | REAPER | after M1.9 |
| The Wine bridge | on a plugin that does nothing, at 32 and 64 frames, at the median and the 99.9th percentile: Magnolia's cost above a futex ping-pong between two processes at most half of yabridge's | yabridge's master branch | M1.7 |
| A plugin's reload | faster than Bitwig Studio's | Bitwig Studio | M1.6 |
| Start, memory, loading | an empty project: Magnolia's own cost in start and memory at most a third of the best rival's; the reference project playable within half the best rival's time to play it, fully loaded never later | REAPER, Bitwig Studio, Ardour | from Stage 3, when Magnolia has a window; M1.8 measures the engine's start beside the rivals' for reference; loading at Stage 2 |
| Interface | Magnolia's own cost above a blank window at most a third of the best rival's: memory (all processes), start, processor, GPU power, a frame's time; each rival at its own frame rate, Magnolia capped to it; under the measure's noise, the blank window's figure | Bitwig Studio, REAPER, Ardour on the screen of reference | Stage 3 |
| Look | judged on written criteria, and blind by at least 20 judges, and preferred | Bitwig Studio, Ableton Live | Stage 3 |
| Listening | in blind tests by at least 20 judges new to Magnolia, their protocol and pass rule written first: time-stretch at least equal; the built-in essentials preferred or not told apart, for less processor per instance | élastique, Ableton Live's Complex Pro; the plugins the best DAWs bundle | Stage 4 |
| Creation | five tasks (a beat from nothing to its export, a vocal comp, a mix to a loudness target, music in step with a video, a game's loop exported as stems), each against its best rival: 4 times less waiting on what Magnolia controls; renders at least as fast; 1.5 times less human time, claimed beyond the spread of balanced sessions, the bias of habit noted; indicative against rivals measured in the virtual machine until the Windows stage | FL Studio, Pro Tools, Ableton Live, REAPER | each task from the stage that brings it |
| Agents | three tasks through MCP, 30 runs each per model: success not significantly below the best rival's by a one-sided exact test at 5 %, median tokens at most half, tool definitions counted ([ADR-0016](adr/0016-agents-on-the-shape-of-uwuis-agent-core.md)) | the community MCP servers of REAPER and Ableton Live | Stage 4's end |
| Accessibility | sessions with blind volunteers, scored on the creation tasks' success, video excepted; at the Windows stage, as often and as fast as REAPER with OSARA under NVDA | REAPER with OSARA | Stage 3; the Windows stage |

**Hours without a crash** are a goal counted over time, not a gate: under one crash per 10,000
hours of use for the engine, under one per 1,000 for the interface, plugins' crashes and CI's hours
apart. The rate observed is published with its upper bound at 95 %, and claimed only once the hours
show it. Hours are counted on the machine and sent only by the user's choice: nothing ticked in
advance, "no" as easy as "yes".

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
| Interface and order | On uwui, after the engine and the project ([ADR-0006](adr/0006-the-interface-on-uwui-after-the-engine-and-the-project.md)); Magnolia a later client of uwui, not one of its first programs ([ADR-0011](adr/0011-magnolia-no-longer-a-program-of-uwuis-first-stage.md)) |
| Engine | Own duplex backends, a compiled graph, exact time ([ADR-0007](adr/0007-the-audio-engine.md)) |
| Time model | Ticks of 1/4,838,400 quarter note, absolute positions in time units of 1/903,168,000,000 s, one rounding rule ([ADR-0018](adr/0018-the-time-model.md)) |
| Project model | Tracks with channels, linked patterns ([ADR-0008](adr/0008-the-project-model.md)) |
| Project file | SQLite, nothing lost ([ADR-0009](adr/0009-the-project-in-sqlite-nothing-lost.md)) |
| Quality bar | Above: floors, and leads over measured rivals ([ADR-0010](adr/0010-the-quality-bar.md), [ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)) |
| Processes | The engine a server; the interface, the command line and MCP its clients; the interface and the plugins' compositor each a process of their own ([ADR-0015](adr/0015-the-interface-in-a-process-of-its-own.md)) |
| Agents | The MCP server on the shape of uwui's agent core, then on the core itself; an agent bench against the rivals' servers ([ADR-0016](adr/0016-agents-on-the-shape-of-uwuis-agent-core.md)) |
| Platforms | Linux first, Windows at a later stage, macOS a candidate stage after Stage 4 ([ADR-0017](adr/0017-macos-a-candidate-stage-after-stage-4.md)) |
| Language | Rust, stable |

### Open decisions

Each is decided by an ADR when its stage needs it, not earlier.

| Topic | Candidates | Decided at |
|---|---|---|
| The unit of plugin isolation, and a late plugin's deadline | one process per plugin; one per track's chain; per tier | Stage 1, on the prototype's figures |
| How often a plugin's state is read when it never reports a change | an interval, against its cost on the host's main thread | Stage 1 |
| The audio device at rest | kept open; released after a while at rest, reopened on play or arming | Stage 1, on the measured cost of each |
| The figures left to measure | a plugin's round trip, the render's factor, the saving's sync and its window, power, a plugin's reload, a plugin host's memory, large sessions, each lead's figure | each at the milestone that measures it |
| How the Windows host talks to the engine | a pure Windows executable; a winelib program | Stage 1, on the prototype's figures |
| Render-ahead of tracks without live input | REAPER's way; the graph scheduler alone | Stage 2, on measures of heavy projects |
| Time-stretch | signalsmith-stretch (MIT), Bungee (MPL-2.0), Rubber Band (GPL-2.0-or-later) | Stage 4 |
| Tempo ramps | steps of constant tempo, exact with the time model's arithmetic; a shape (linear in time or in beats) with a rounding rule of its own, as Ardour computes in floating point ([ADR-0018](adr/0018-the-time-model.md)) | the stage that first edits tempo |
| Video decoding | uwui's video crate, FFmpeg for ProRes and DNxHD | the film stage |
| Exchange with film editors (AAF) | none; AAF export | the film stage |
| Wwise and FMOD exports | files and metadata each tool imports | the film and games stage |
| Stem separation | its model and its weights' license | its stage |
| ARA under Wine | research; Windows only | after ARA on Windows |
| The order of the stages after Stage 4 | film and games, the sampler and drum machine, plugin GUIs inside the window, Windows, macOS, stems and ARA | the end of Stage 4 |
| Magnolia's look | uwui's Pro theme as it is; a theme of Magnolia's own that extends Pro; never recognisable as uwui's by its look alone | Stage 3 |
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
| Windows | A platform of its own stage |
| macOS | A candidate stage after Stage 4 ([ADR-0017](adr/0017-macos-a-candidate-stage-after-stage-4.md)) | Linux first |

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
in low-latency mode) play and record in a test session at 64 frames on the high tier for eight
hours without xrun or missed deadline, the worst block under 85 % of the period, and each survives
the kill of its process; the figures are in `docs/measurements.md`.

### Stage 2 — The project → 0.2

**Goal.** A whole project, created, edited, saved and recovered without an interface.

- The model of [ADR-0008](adr/0008-the-project-model.md): tracks and channels, multi-output and
  multi-timbral instruments, routing, linked patterns, take lanes, automation, the tempo map.
- The SQLite file, its migrations, every edit a command, undo kept, snapshots.
- Recovery after simulated power cuts.
- Offline render, stems, loops with their points, MIDI files in and out.
- Large sessions: 1,000 tracks and 50,000 clips, measured.

**Exit.** A project of the reference's size is built by commands, survives a simulated power cut at
any gesture with nothing lost, and renders offline to stems that null against real-time playback.

### Stage 3 — The interface → 0.3

**Goal.** The owner makes a beat from nothing to an exported file in Magnolia, on uwui.

**Starts when** a release of uwui, its API declared stable, holds the set that
[`uwui-needs.md`](uwui-needs.md#what-stage-3-needs-first) names first; no date is set
([ADR-0011](adr/0011-magnolia-no-longer-a-program-of-uwuis-first-stage.md)).

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
- macOS, measured on an Apple Silicon machine, with Audio Units, signing and notarisation, plugin
  windows floating ([ADR-0017](adr/0017-macos-a-candidate-stage-after-stage-4.md)).
- 1.0: the whole of [ADR-0001](adr/0001-what-magnolia-is.md), audited.

## 5. Release map

| Version | Stage | Content |
|---|---|---|
| 0.1 | 1 | The engine and the plugins, without an interface |
| 0.2 | 2 | The project, without an interface |
| 0.3 | 3 | The interface |
| 0.4 | 4 | Recording and audio editing |
| 1.0 | Later | Everything ADR-0001 asks |
