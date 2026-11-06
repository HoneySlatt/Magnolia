# 0001. What Magnolia is

- Status: Accepted
- Date: 2026-11-06

## Context

The owner produces music for film, video games and YouTube. They make the beat, record vocals, mix
and master. Today they compose in FL Studio and record, edit and mix in Pro Tools, both on Windows.
They find no DAW on Linux that is truly good. No DAW hosts Windows VST3 or CLAP plugins on Linux by
itself: Bitwig and REAPER need yabridge, installed and set up apart; LMMS bridges only the old VST2
format ([the survey](../survey.md#the-wine-bridge)).

In this ADR, FL Studio's *playlist* is its arrangement; Pro Tools' *playlists* are a track's
alternate takes ([glossary](../glossary.md)).

The owner answered an interview on 2026-11-06, one theme at a time. This ADR records what they
asked. The decisions on how to build it are the ADRs that follow.

## Decision

**Magnolia is a DAW of the class of FL Studio and Pro Tools**, written in Rust. It aims at the top
of the field as a whole, not on one feature.

**What it keeps from each.** From FL Studio: the piano roll (ghost notes, chord stamps, scales,
its tools), patterns and the playlist, the step sequencer, and the speed of an idea. From Pro Tools:
audio editing, playlists and comping, mixing, and solidity on large sessions.

**What it avoids.** FL Studio's weak audio recording, its floating windows everywhere and its
confusing mixer routing. Pro Tools' weak MIDI, its single plugin format and its absence on Linux.

**Plugins.** It hosts VST3 and CLAP, and Windows plugins on Linux through Wine, built in. The
plugins that must work: Auto-Tune, Serum, FabFilter, Valhalla, Kontakt and Native Instruments'
libraries. Melodyne through ARA2 is wanted. VST2 is not hosted: its SDK is no longer licensed. If a
plugin crashes, Magnolia survives.

**Film and games.** A video track synced to the music (H.264, H.265, ProRes, DNxHD), SMPTE
timecode, sync points on picture, tempo and meter changes. Loops that repeat without a click, with
their loop points; stems and layers; exports for Wwise and FMOD. Mastering on the master bus, and in
a separate document for an album or a soundtrack.

**Recording.** One or two sources at a time, through a Focusrite interface; a MIDI keyboard and
pads. Vocals with Auto-Tune live in the headphones, without felt delay.

**Saving and exchange.** Nothing is ever lost: every gesture is kept, through a crash or a power
cut. Exchange with other DAWs is by stems and MIDI files. The owner works alone and sends files.

**Interface.** Dark, dense and refined. One window with docked panels, detachable to other
monitors; the owner uses two or more. Plugin GUIs inside the window, not floating. Full
accessibility from the start: everything by the keyboard, every control read by a screen reader,
levels read aloud.

**Built in.** Mixing essentials (EQ, compressor, gate, limiter, reverb, delay, loudness meter), a
sampler and a drum machine for the step sequencer, and stem separation run locally, its large model
installed on demand.

**Platforms.** Linux first; Windows at a later stage; macOS kept within reach, without a port.

**Machines.** The high tier is the owner's desktop at 4K and 240 Hz. On the owner's very modest
laptop (2 cores, 4 GB), every feature works, with fewer tracks; only detail is lost, never
fluidity. Large orchestral libraries will not fit in its memory.

**Distribution.** Free software, binaries for sale, as Ardour does
([ADR-0002](0002-gpl-3-0-or-later-copyright-kept.md)). The repository is public from its first
commit.

**Nothing else around it is planned.** No phone remote, no plugins for other DAWs, no sample
manager.

## Alternatives considered

- **A DAW in the FL Studio model alone, or in the Pro Tools model alone**: less to build, but the
  owner would keep needing two programs.
- **Exchange through DAWproject, AAF or FL Studio's project files**: the owner needs none today.
  AAF may come back with film work; it is an open decision in the
  [ROADMAP](../ROADMAP.md#open-decisions).
- **Online collaboration**: a server and synchronisation, for a need the owner does not have.

## Consequences

- Windows plugins are the project's largest risk. The owner chose not to test them in a stage of
  their own first ([ADR-0004](0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
- Film work brings video decoding, a precise time model and large media files from the start of
  the engine's design.
- Accessibility from the start means every control drawn by Magnolia is described for screen
  readers when it is written, not later.
