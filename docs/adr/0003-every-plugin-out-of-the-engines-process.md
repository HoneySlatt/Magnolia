# 0003. Every plugin out of the engine's process

- Status: Accepted
- Date: 2026-11-06

## Context

The owner wants Magnolia to survive any plugin's crash ([ADR-0001](0001-what-magnolia-is.md)).
Hosts differ ([the survey](../survey.md#plugin-isolation)):

- Bitwig offers five modes, from all plugins inside Bitwig to each plugin in its own process. Its
  default puts all plugins together in one process apart from the engine.
- REAPER can run a plugin in a separate or a dedicated process.
- Ardour refuses out-of-process hosting because each call costs at least two context switches.

The cost has two parts, both estimated, none measured on Magnolia's machines. **Memory**: a Wine
process with a JUCE plugin may take 50 to 150 MB; twenty of them would take 1 to 3 GB, most of the
lowest tier's memory. **Time**: a futex round trip costs 1 to 2 µs in microbenchmarks, and may cost
tens of µs in a real session on a slow core, its caches cold. Plugins in
series in separate processes are called one after another: a six-plugin vocal chain makes six
round trips per period. At 128 frames and 48 kHz a period lasts 2.67 ms.

## Decision

**No third-party plugin runs in the engine's process.** Each runs in a plugin-host process, native
or under Wine. Magnolia's own DSP, its built-in effects and instruments, runs in the engine.

**The unit of isolation is chosen on measurement**, for each hardware tier. A prototype in Stage 1
measures one process per plugin against one process per track's chain, in memory and in time per
period. Its figures set the default. One process per plugin stays available for a plugin the user
marks as fragile.

**A plugin that crashes or answers late outputs silence.** Late means past a deadline, about 70 %
of the period, measured and set in Stage 1. The block is silent, the slot is marked, and the engine
does not wait. Silence, not bypass: a bypassed limiter could send a loud signal to the headphones.
A crashed plugin can be reloaded.

**Its last state is kept.** The host takes a snapshot of a plugin's state when the plugin reports
that it changed (CLAP's `mark_dirty`, VST3's `setDirty`). Many plugins never report it; their state
is read at intervals instead, on the host's main thread, at a cost measured in Stage 1, which sets
the interval. The host also keeps the parameter changes since the last snapshot. A reloaded plugin
returns to that state.

## Alternatives considered

- **One process per plugin, always**: a crash touches one plugin. Rejected as a fixed default: the
  memory and the round trips on the lowest tier.
- **One process per track's chain, fixed now**: light. Rejected as a fixed default: a crash
  silences the whole track, and the figures that would justify it are unknown.
- **Plugins inside the engine's process, as Ardour does**: the lightest, but a crash takes the
  session down, against the owner's requirement.

## Consequences

- The engine never blocks on a plugin: a late or dead plugin costs a silent block, not an xrun.
- Each round trip has a budget, measured and checked ([ROADMAP](../ROADMAP.md#quality-bar)).
- The promise "nothing is lost" holds for a plugin's internal state only to its last snapshot
  ([ADR-0009](0009-the-project-in-sqlite-nothing-lost.md)).
- The Wine bridge is the same host, built for Windows
  ([ADR-0004](0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
