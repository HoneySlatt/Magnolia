# 0008. The project model: tracks with channels, linked patterns

- Status: Accepted
- Date: 2026-11-06

## Context

The owner wants FL Studio's patterns, playlist and step sequencer, and Pro Tools' clear tracks,
playlists, comping and mixing ([ADR-0001](0001-what-magnolia-is.md)). They use both
multi-instrument patterns (a verse with drums, bass and chords) and one instrument per pattern.

In FL Studio, playlist lanes are apart from mixer channels: a channel is routed to a mixer insert
by hand. That freedom is what makes its routing confusing. Pro Tools, Studio One, Bitwig and
Ableton tie each track to a mixer channel. Studio One gives a multi-output instrument child
channels under its track.

## Decision

**Each track owns its mixer channel.** What plays on the "Bass" track goes out of the "Bass"
channel. A multi-output instrument (Kontakt with 16 outputs, a drum machine with one output per
pad) has child channels under its track. Several MIDI tracks may feed one multi-timbral instrument.
Beneath the tracks, an explicit routing graph carries sends, buses and sidechains.

**A pattern is a reusable block of notes and automation.** Placed several times, its instances are
linked: an edit in one appears in all. Each instance has its own start offset, length, loop,
transposition and velocity scale. "Make unique" detaches an instance, copying it on write.

**A pattern may span several instrument tracks.** A pattern lane above the arrangement paints
patterns into it, as FL Studio's playlist does; each instance places a linked clip on each of its
tracks. Moving, splitting or deleting one member of a multi-track instance follows written rules,
designed in Stage 2.

**The step sequencer is a view of the selected pattern**: a row per instrument, steps to click.

**Playlists, take lanes and comping are in the model from Stage 2**, though their interface comes
at Stage 4. They decide how clips are owned, which cannot change later without a migration.

**Mastering an album or a soundtrack is a document of its own**: a sequence of rendered pieces,
with its own model.

## Alternatives considered

- **FL Studio's model**: the owner's habits exactly, but its routing is what they dislike.
- **Clips on tracks, as Ableton and Bitwig, without multi-instrument patterns**: simpler, but a
  verse could no longer be placed in one stroke.

## Consequences

- The model is richer than either program's; Stage 2 designs its rules and tests them headless.
- The mixer's routing is visible from the tracks, with the routing graph for what tracks do not
  show.
