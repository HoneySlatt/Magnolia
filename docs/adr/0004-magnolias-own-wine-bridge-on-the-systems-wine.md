# 0004. Magnolia's own Wine bridge, on the system's Wine

- Status: Accepted
- Date: 2026-11-06

## Context

Every plugin the owner named is a Windows plugin ([ADR-0001](0001-what-magnolia-is.md)). On
Linux they run through Wine. The state of the field, verified on 2026-11-06
([the survey](../survey.md#the-wine-bridge)):

- yabridge is the bridge that Bitwig's and REAPER's users install. It is GPL-3.0-or-later.
- yabridge's last release, 5.1.1 (2024), does not work with Wine 9.22 and later; its master
  branch, still active, has fixes that are not yet released.
- yabridge runs one Wine process per plugin. Audio goes through shared memory, control through
  Unix sockets. Its GUIs are X11 windows reparented into the host's, Wine kept off Wayland.
- When a yabridge host process dies, the DAW dies with it (yabridge issue #534, open).
- Wine 11.0 is stable since January 2026. Its Wayland driver cannot embed a window into another
  program's. It maps thread priorities to nice values, not to real-time scheduling, as the survey
  read it; M1.7 checks this in Wine's code before the design relies on it.
- No DAW hosts Windows VST3 or CLAP plugins through a bridge of its own. LMMS has one, for VST2
  only.

Plugins reported fragile under recent Wine: Native Access, Kontakt's installer; Serum 2 on
unpatched Wine. Reported but not verified: plugins built with JUCE 8, which draws through Direct2D,
whose Wine support is incomplete; plugins protected by iLok.

## Decision

**Magnolia has its own bridge, in Rust**, part of the plugin-host design of
[ADR-0003](0003-every-plugin-out-of-the-engines-process.md). The same host program, built for
Windows, runs under Wine and hosts Windows VST3 and CLAP plugins.

**The system's Wine is used**, not a copy bundled with Magnolia. Magnolia keeps a list of the Wine
versions that passed its tests. It says plainly when it finds another, and which plugins may fail.

**A prototype chooses how the Windows host talks to the Linux engine.** Two ways are compared, on
their round-trip time per period and their reliability: a pure Windows executable, and a winelib
program that calls both Windows and Linux, as yabridge does. yabridge's master branch is the
baseline the prototype must beat; it is a measuring tool, never shipped. A third way, a Windows
executable with a Linux library through Wine's internal Unix calls, is set aside: that interface
changes between Wine versions, which the system's Wine does not pin.

**The Windows host's audio thread runs with real-time priority**, raised from the Linux side
(through rtkit or the scheduler directly). Without it, the engine's real-time thread would wait on a
normal-priority thread, and xruns under load would be certain.

**The owner's plugins are the bridge's tests.** The bridge's milestone is done only when Serum,
FabFilter's plugins, a Valhalla reverb, Kontakt with a large library and Auto-Tune in low-latency
mode each load, play, show their GUI and survive a kill of their process, measured. The owner runs
these tests by hand, on each Wine version that the list adds, and activates the licenses themselves,
in a test prefix. CI cannot hold licensed plugins: it runs the same tests on Windows test plugins
that the project builds.

**Melodyne through ARA under Wine is research.** ARA lets a plugin read the host's audio from its
own threads and keep a document in step with the host's. Nobody has bridged it across processes.
It is promised first on Windows, where no bridge is needed.

## Alternatives considered

- **Shipping yabridge as it is**: quick, but its release is broken on recent Wine, a crash kills the
  DAW, and nothing can embed its windows. Not "built in and reliable".
- **Forking yabridge's C++**: a mature protocol, but two languages, and a design shaped as a plugin
  library loaded in the host, against ADR-0003.
- **A Wine bundled and pinned by Magnolia**: an update of the system could not break a plugin.
  The owner chose the system's Wine: Magnolia stays lighter, by about 200 MB, and follows the
  distribution's updates.
- **A stage of its own first, testing the owner's plugins under Wine before any code**: the
  largest risk removed before any work. The owner judged it unneeded. The risk stays; the bridge's
  milestone carries those tests, as early as possible in Stage 1.

## Consequences

- An update of the system's Wine can break a plugin; Magnolia detects it and says so, it cannot
  prevent it. CI runs the test plugins on the Wine versions of the main distributions.
- A plugin that fails under Wine may make the owner's sessions unusable on Linux. That is learnt in
  Stage 1, before the project model and the interface are built.
- Ideas and code from yabridge may be reused, with their notices
  ([ADR-0002](0002-gpl-3-0-or-later-copyright-kept.md)).
