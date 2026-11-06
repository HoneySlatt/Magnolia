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
