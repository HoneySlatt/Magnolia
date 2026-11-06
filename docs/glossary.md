# Glossary

The terms Magnolia coins, and the audio and system terms its documents use without explaining.

- **ARA**: an interface through which a plugin such as Melodyne reads a clip's whole audio and
  edits it in place.
- **Arena**: the store where the engine's nodes live, apart from any compiled graph, so that they
  survive a swap.
- **Arrangement**: the timeline of a song's tracks and clips; FL Studio calls it the *playlist*.
- **Block**: the frames the engine processes in one callback, a period's worth.
- **Channel**: a strip of the mixer. Each track owns one; a multi-output instrument has child
  channels under its track ([ADR-0008](adr/0008-the-project-model.md)).
- **Collector**: the thread that frees what the real-time thread let go, so that it never frees
  memory itself.
- **Comping**: building one take from the best parts of several.
- **Compiled graph**: the order of the engine's nodes, their buffers and their compensating delays,
  built off the real-time thread and swapped in at a block's boundary
  ([ADR-0007](adr/0007-the-audio-engine.md)).
- **Deadline**: the share of a period past which a plugin's answer is not awaited and its block is
  silent ([ADR-0003](adr/0003-every-plugin-out-of-the-engines-process.md)).
- **Drop-frame**: a timecode at 29.97 frames a second that skips frame numbers to stay in step with
  the clock.
- **Duplex**: input and output of a period handled in one callback.
- **Gesture**: one action of the user that changes the project, such as a note drawn or a clip
  moved; one transaction in the project's file.
- **Group commit**: gestures that arrive during a sync to disk, written together at the next one.
- **Linked instance**: one placement of a pattern; an edit in one shows in all, except its own
  offset, length, loop, transposition and velocity scale.
- **MCP**: the Model Context Protocol, through which agents drive Magnolia.
- **Null**: two renders that cancel exactly when one is subtracted from the other.
- **OSARA**: an extension that makes REAPER usable with a screen reader; Magnolia's reference for
  accessibility.
- **Pattern**: a reusable block of notes and automation, over one or several instrument tracks.
- **Pattern lane**: the lane above the arrangement where patterns are painted.
- **Period**: the number of frames the audio device asks for at each callback, 64 or 128 here.
- **Playable, fully loaded**: the two moments of opening a project: it plays, its plugins still
  loading as silent slots; then every plugin is loaded.
- **Playlist**: in Magnolia, as in Pro Tools, one of a track's alternate sets of clips. FL Studio's
  playlist is Magnolia's arrangement.
- **Plugin host**: a process apart from the engine that runs third-party plugins, native or under
  Wine.
- **Prefix**: a folder holding one Windows installation for Wine, its programs and its registry.
- **Reference project**: a session fixed in M1.3 for each tier, on which the sound's budgets are
  measured, never tuned for afterwards.
- **Render-ahead**: rendering tracks without live input before the playhead reaches them, on worker
  threads, as REAPER does.
- **Rest**: the transport stopped, nothing armed or monitored, plugin GUIs closed. Magnolia then
  draws nothing, plugin hosts receive no calls, and nothing of Magnolia's wakes outside the audio
  device's callback, while the device stays open
  ([ADR-0010](adr/0010-the-quality-bar.md)).
- **rtkit**: a system service that grants real-time priority to a program's threads.
- **Screen of reference**: a screen fixed before the work, on which uwui and its rivals are
  measured.
- **Snapshot of a plugin's state**: its state saved when it reports a change, or read at an
  interval when it never does.
- **Stems**: a mix exported as several files, one per group of tracks, that sum to the mix.
- **Take lane**: one recorded take of a track, kept beside the others for comping.
- **Test session**: Stage 1's description of a session, read only by tests and the command line.
- **Tier**: a class of machine with its own budgets; the high tier is the owner's desktop, the
  lowest tier a two-core laptop.
- **WAL**: SQLite's write-ahead log, which makes each transaction whole or absent after a crash.
- **Winelib**: a program built for Linux that also calls Windows' functions through Wine.
- **Wine bridge**: the plugin host built for Windows, run under the system's Wine
  ([ADR-0004](adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
- **Xwayland**: the X11 server that runs X11 programs inside a Wayland session.
