# The field, surveyed on 2026-11-06

What the design of 2026-11-06 found about DAWs, plugins and audio on Linux. It is the ground of
ADRs 0001 to 0010. Facts change: each one carries its source, and those marked *unverified* came
from secondary sources only. This file is not updated as the field moves; a later survey gets a
file of its own.

## The DAWs

| DAW | Version | Linux | What users praise | What they criticise |
|---|---|---|---|---|
| FL Studio | 2026 | no; Wine only | piano roll, patterns and playlist, step sequencer | audio recording and comping, mixer routing, dense windows |
| Pro Tools | 2026.11 | no | editing, playlists and comping, large sessions, delay compensation | subscription, weak MIDI, AAX only |
| Ableton Live | 12.4 | no | session view, warping | tracking and comping, no ARA |
| Logic Pro | 12 | no (macOS) | Flex Time and Pitch, Session Players | Mac only |
| Bitwig Studio | 6.1 | yes | modulators, the Grid, note expressions, plugin sandboxing, CLAP | audio editing, no ARA |
| REAPER | 7.85 | yes | lightness, routing, customisation, render-ahead | dated look, learning curve |
| Ardour | 9.8 | yes (GPL-2.0) | routing, recording, sessions | GTK2, no Wayland, plugin GUIs at HiDPI |
| Zrythm | 2.0 alpha | yes | — | a rewrite in C++ and Qt, not for production |
| LMMS | 1.3 alpha | yes | free, FL-like | no stable release since 2020 |
| Fender Studio Pro (Studio One) | 8 | public beta, *unverified* | arranger track, scratch pads, drag and drop, DAWproject, ARA | an uncertain rebrand |

Sources: [Image-Line news](https://www.image-line.com/news),
[Pro Tools 2026.11](https://www.soundonsound.com/news/avid-introduce-pro-tools-202611),
[Live 12 release notes](https://www.ableton.com/en/release-notes/live-12/),
[Bitwig 6](https://musictech.com/news/gear/bitwig-studio-6/),
[REAPER](https://www.reaper.fm/whatsnew.txt), [Ardour 9.0](https://ardour.org/news/9.0.html),
[Zrythm changelog](https://github.com/zrythm/zrythm/blob/master/CHANGELOG.md),
[LMMS](https://lmms.io/news/2026-09-06),
[Fender Studio Pro](https://www.soundonsound.com/news/studio-one-becomes-fender-studio-pro).

On Linux, producers lack Windows plugins and rely on yabridge; native DAWs embed plugin GUIs through
X11, so they run under Xwayland and blur at fractional scales
([Splitwave #66](https://github.com/Horuse/Splitwave/pull/66),
[REAPER on Wayland](https://forum.cockos.com/showthread.php?t=282844)).

Bitwig draws its interface on the GPU since 5.2
([Bitwig](https://www.bitwig.com/stories/new-compressor-and-eqs-hit-bitwig-studio-52-304/)).
FL Studio's piano roll is described in its [manual](https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/html/pianoroll.htm).
REAPER's peak files, a pyramid of up to 16 levels of detail, are documented in
[reapeaks.txt](https://www.reaper.fm/sdk/reapeaks.txt).

## Licenses

- **VST3 SDK**: MIT since 3.8.0, 2025-10-20; no agreement needed; the VST name and logo optional,
  under usage guidelines. 3.8 adds a preview Wayland API (`IWaylandHost`).
  [LICENSE.txt](https://github.com/steinbergmedia/vst3sdk/blob/master/LICENSE.txt),
  [release](https://forums.steinberg.net/t/vst-3-8-0-sdk-released/1011988),
  [iplugview.h](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/master/gui/iplugview.h).
- **VST2**: its SDK is proprietary and no longer licensed since 2018.
- **ASIO SDK 2.3.4**: GPL version 3 or Steinberg's proprietary license, countersigned.
  [LICENSE.txt mirror](https://github.com/audiosdk/asio/blob/master/LICENSE.txt),
  [KVR](https://www.kvraudio.com/news/steinberg-moves-vst-3-sdk-to-mit-open-source-license-asio-now-gplv3-65179).
- **CLAP**: MIT, 1.2.10. FabFilter ships CLAP; Serum, Valhalla and Auto-Tune do not, as far as
  found. [clap](https://github.com/free-audio/clap),
  [FabFilter](https://www.fabfilter.com/news/1702983600/all-plug-ins-updated-with-clap-support-bug-fixes-and-more).
- **ARA SDK**: Apache-2.0, 2.3.0. [ARA_SDK](https://github.com/Celemony/ARA_SDK).
- **yabridge**: GPL-3.0-or-later. **Wine**: LGPL-2.1-or-later.
- **Time-stretch**: signalsmith-stretch MIT; Bungee MPL-2.0; Rubber Band GPL-2.0-or-later or
  commercial; zplane élastique commercial.
- **DAWproject**: MIT, version 1.0; read by Bitwig, Studio One and Cubase.
  [dawproject](https://github.com/bitwig/dawproject).

## The Wine bridge

- yabridge 5.1.1, its last release (2024), fails on Wine 9.22 and later; master is active, fixes
  unreleased. One Wine process per plugin; Unix sockets and shared memory; X11 reparenting;
  `WAYLAND_DISPLAY` unset. A dead host terminates the DAW
  ([#534](https://github.com/robbert-vdh/yabridge/issues/534)).
  [Architecture](https://github.com/robbert-vdh/yabridge/blob/master/docs/architecture.md).
- Fragile plugins: Native Access, Serum 2 on stock Wine, iZotope's activation, Waves V13; iLok
  *unverified*. JUCE 8 draws through Direct2D, which Wine supports partly *(unverified)*.
- Wine 11.0 is stable since 2026-01-13; ntsync from Linux 6.14; thread priorities as nice values
  only, per its release notes as read *(to verify in Wine's code)*; its Wayland driver cannot embed into another program.
  [wine](https://github.com/wine-mirror/wine).
- No DAW hosts Windows VST3 or CLAP plugins through a bridge of its own. LMMS runs Windows VST2
  plugins through a Wine host of its own. Carla (GPL-2.0-or-later) has Wine bridges; LinVst is
  GPL-3.0; airwave is abandoned.

## Plugin isolation

- Bitwig: five modes, from within Bitwig to each plugin apart; default "together"; a crashed plugin
  offers a reload. [User guide](https://www.bitwig.com/userguide/latest/vst_plug-in_handling_and_options/).
- REAPER: a plugin in a separate or a dedicated process *(2012 sources)*.
- Ardour refuses out-of-process hosting, citing two context switches per call.
  [Ardour](https://ardour.org/plugins-in-process.html).
- A futex round trip costs 1 to 2 µs in microbenchmarks
  ([Bendersky](https://eli.thegreenplace.net/2018/measuring-context-switching-and-memory-overheads-for-linux-threads/));
  nothing is measured in a DAW.

## Plugin GUIs on Wayland

- VST3 and CLAP embed through X11 (XEmbed); CLAP recommends floating windows on Wayland
  ([gui.h](https://github.com/free-audio/clap/blob/main/include/clap/ext/gui.h)).
- VST3 3.8's preview Wayland API makes the host a small compositor.
- yabridge's maintainer judges embedding Wine windows into a Wayland host "a massive amount of
  effort" ([#406](https://github.com/robbert-vdh/yabridge/issues/406)).

## The audio engine

- REAPER, Bitwig and Ardour use the JACK API or ALSA on Linux; none uses PipeWire's native API.
  PipeWire is at 1.6.11; JACK2's last release is 1.9.22 (2023).
- PREEMPT_RT is in mainline since Linux 6.12; desktop kernels rarely enable it. PipeWire's RT
  module falls back to rtkit.
- REAPER renders tracks ahead by about 200 ms on worker threads; live tracks bypass it
  ([Frankel](https://gearspace.com/board/q-a-with-justin-frankel-designer-of-reaper-/119731-buffers-latency-reaper.html)).
- Ardour's graph runs on N threads triggered by reference counts
  ([graph.cc](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/graph.cc)).
- Tracktion Graph inserts latency nodes when the graph is built.
- A Scarlett 2i2 3rd gen measured about 6.5 ms round trip at 64 frames on PipeWire
  ([Arch forum](https://bbs.archlinux.org/viewtopic.php?id=270879)); an RME Babyface Pro FS about
  4 ms on Windows ([RME forum](https://forum.rme-audio.de/viewtopic.php?id=42395)).
- Rust crates (crates.io, 2026-11-06): clack 0.2 (CLAP host, MIT/Apache), vst3 0.3 (MIT/Apache
  bindings), cpal 0.18 (separate input and output streams), jack 0.13, pipewire 0.10, rtrb 0.4,
  basedrop 0.1, rtsan-standalone 0.3 (RealtimeSanitizer).

## Rust DAWs

Meadowlark, a DAW in Rust, stalled; its author blames the interface: no Rust toolkit then had
damage tracking, a locked pointer or several windows, and the work was unfunded
([blog](https://billydm.github.io/blog/daw-frontend-development-struggles/)). Its engine, Dropseed,
is on [Codeberg](https://codeberg.org/Meadowlark/Dropseed). No maintained, production-grade Rust DAW
was found.

## Saving

- Ardour: a session folder, XML, backups, undo history kept, snapshots.
- REAPER: a text file of chunks, plugin state in base64, a `.rpp-bak` at each save.
- Bitwig 6 keeps a backup when it opens a project from an earlier version.
- FL Studio and Pro Tools save every few minutes.

## Metering

ITU-R BS.1770-5 (2023) defines loudness; EBU R128 sets −23 LUFS and −1 dBTP for broadcast
([R128](https://tech.ebu.ch/docs/r/r128.pdf)).

## Accessibility

REAPER with OSARA speaks tracks, items, positions, markers, plugin parameters and levels, with NVDA
and VoiceOver ([OSARA](https://osara.reaperaccessibility.com/)). Pro Tools works with VoiceOver.
Blind producers need every action by the keyboard, every value spoken, and plugin parameters as
lists ([ACM 2025](https://dl.acm.org/doi/full/10.1145/3733606)).

## The market measured, 2026-11-10

Gathered for [ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md) to
[ADR-0016](adr/0016-agents-on-the-shape-of-uwuis-agent-core.md).

### Processes

- Bitwig Studio installs its audio engine as an executable of its own, `BitwigAudioEngine`, beside
  `BitwigStudio` and its plugin hosts ([a packaging issue](https://github.com/yioannides/bitwig-fedora/issues/5),
  [a CLAP interop report](https://github.com/free-audio/interop-tracker/issues/65)). Its user guide
  says plugins hosted apart from the engine leave the engine running when they crash
  ([Bitwig's user guide](https://www.bitwig.com/userguide/latest/vst_plug-in_handling_and_options)).
  That the engine survives a forced close of the interface comes from a community answer only; no
  page of Bitwig's says so.
- Ardour runs its engine and its interface in one process; the engine does not know the interface,
  which allows a build without one ([Paul Davis, 2026-05-30](https://discourse.ardour.org/t/113356)).
- REAPER runs plugins inside its own process unless a plugin is set to run apart; this survey did
  not read REAPER's documentation again for it.

### Servers for agents

No DAW maker ships an MCP server or an API for agents. Community servers, with their stars and last
commit on 2026-11-10:

| DAW | Server | Stars | Last commit | Through |
|---|---|---|---|---|
| Ableton Live | [ahujasid/ableton-mcp](https://github.com/ahujasid/ableton-mcp) | 3,334 | 2026-09-22 | a Remote Script; telemetry on by default |
| REAPER | [bonfire-systems/reaper-mcp](https://github.com/bonfire-systems/reaper-mcp) | 184 | 2026-10-25 | python-reapy, 61 tools |
| REAPER | [TwelveTake-Studios/reaper-mcp](https://github.com/TwelveTake-Studios/reaper-mcp) | 104 | 2026-11-09 | a Lua script inside REAPER, 188 tools |
| Bitwig Studio | [WeModulate/bitwig-mcp-server](https://github.com/WeModulate/bitwig-mcp-server) | 79 | 2025-04-10 | OSC |
| Bitwig Studio | [fabb/WigAI](https://github.com/fabb/WigAI) | 62 | 2026-10-18 | an extension; archived |

Smaller servers exist for FL Studio, Logic Pro, Pro Tools and Ardour. One benchmark of agents
driving REAPER, TOMORI-BENCH, was withdrawn by its authors on 2026-11-06.

### Accessibility

- OSARA runs on Windows and macOS, with NVDA and VoiceOver
  ([OSARA](https://github.com/jcsteh/osara)). An experimental port to Linux speaks through Orca, but
  not yet REAPER's drawn views ([osara-linux](https://audio.codyhurst.com/accessibility)).
- "By far the most popular among blind users is Reaper"
  ([Sound Without Sight, 2023](https://soundwithoutsight.org/hub-articles/basics-mixing-and-daws/)).
  A study of 2020 scored REAPER with OSARA above Pro Tools and Cubase
  ([Pedrini et al.](https://www.scitepress.org/Papers/2020/101670/101670.pdf)).
- Ableton Live 12 supports screen readers on macOS and Windows, with gaps: Max for Live, MPE editing,
  the browser's filters ([Live 12's manual](https://www.ableton.com/en/live-manual/12/accessibility-and-keyboard-navigation/)).
- Ardour's timeline and mixer are drawn by Ardour itself, so Orca cannot read them
  ([Ardour's forum, 2021](https://discourse.ardour.org/t/please-consider-screenreader-accessibility/106722)).

### Modest hardware

No figure is published for the tracks or plugins a DAW holds on a two-core laptop. Bitwig Studio
asks for a processor with AVX2 and 4 GB of memory ([Bitwig](https://www.bitwig.com/download/));
REAPER and Ardour state no minimum.

## Time models, 2026-11-11

Surveyed for M1.2, the ground of [ADR-0018](adr/0018-the-time-model.md). Logic, Cubase, Studio One,
Bitwig and Ableton Live publish nothing on their resolution or their tempo's storage that could be
found.

- Pro Tools counts 960,000 pulses per quarter note and shows 960; "some amount of sample-rounding may
  occur" ([Pro Tools Reference Guide 2025.12](https://resources.avid.com/SupportFiles/PT/Pro_Tools_Reference_Guide_2025.12.pdf)).
  It offers 19 timecode rates: 23.976, 24, 25, 29.97, 29.97 drop, 30, 30 drop, 47.952, 48, 50,
  59.94, 59.94 drop, 60, 60 drop, 100, 119.88, 119.88 drop, 120 and 120 drop. Its tempo curves are
  stored as steps.
- Ardour counts 1,920 ticks per beat
  ([types.h](https://raw.githubusercontent.com/Ardour/ardour/master/libs/temporal/temporal/types.h))
  and its absolute time in superclocks, 282,240,000 a second
  ([enums.cc](https://raw.githubusercontent.com/Ardour/ardour/master/libs/temporal/enums.cc)). A
  tempo is a whole number of superclocks per note, the typed BPM rounded once
  ([tempo.h](https://raw.githubusercontent.com/Ardour/ardour/master/libs/temporal/temporal/tempo.h));
  each tempo point keeps its own anchor. Its ramps are linear in beats, computed in floating point
  and rounded to a superclock
  ([tempo.cc](https://raw.githubusercontent.com/Ardour/ardour/master/libs/temporal/tempo.cc)).
- REAPER gives new MIDI items 960 ticks per quarter note
  ([User Guide 7.85c](https://www.reaper.fm/userguide/ReaperUserGuide785c.pdf)); its scripting
  interface passes positions and tempos as doubles.
- Tracktion Engine holds positions in seconds and beats as doubles, and tempos from 20 to 300 BPM
  ([tracktion_Time.h](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_core/utilities/tracktion_Time.h),
  [tracktion_TempoSetting.h](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_engine/model/edit/tracktion_TempoSetting.h)).
- CLAP passes beat time as a 64-bit integer, 2^31 to a quarter note, and its tempo as a double; VST3
  passes quarter notes and tempo as doubles
  ([fixedpoint.h](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/fixedpoint.h),
  [events.h](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/events.h),
  [ivstprocesscontext.h](https://raw.githubusercontent.com/steinbergmedia/vst3_pluginterfaces/master/vst/ivstprocesscontext.h)).
- A Standard MIDI File holds at most 32,767 ticks per quarter note and its tempo as whole
  microseconds per quarter note ([the format, described](http://www.somascape.org/midi/tech/mfile.html)).
- Drop-frame at 29.97 skips labels 0 and 1 at the start of each minute but every tenth
  ([SMPTE timecode](https://en.wikipedia.org/wiki/SMPTE_timecode), citing SMPTE ST 12-1). 59.94
  drop-frame is defined by SMPTE ST 12-3; FFmpeg skips four labels a minute and refuses drop-frame at
  23.976 ([timecode.c](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavutil/timecode.c)).
  The standards' own texts were not read: *unverified* beyond these sources.
- A flick is 1/705,600,000 s, a whole fraction of common frame and sample rates
  ([flicks](https://github.com/facebookarchive/flicks)).
