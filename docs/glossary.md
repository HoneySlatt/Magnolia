# Glossary

The terms Magnolia coins, and the audio and system terms its documents use without explaining.

- **Agent bench**: fixed tasks given through MCP to the same agents, in the same words, with
  Magnolia and with its rivals' servers; success, tries, tokens and time measured
  ([ADR-0016](adr/0016-agents-on-the-shape-of-uwuis-agent-core.md)).
- **ARA**: an interface through which a plugin such as Melodyne reads a clip's whole audio and
  edits it in place.
- **Arena**: the store where the engine's nodes live, apart from any compiled graph, so that they
  survive a swap.
- **Arrangement**: the timeline of a song's tracks and clips; FL Studio calls it the *playlist*.
- **BBT**: a musical position in bars, beats and ticks.
- **Best rival**: on a criterion, the rival that does best, measured; a lead is a margin over it
  ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Blank window**: a window that only clears and presents. What a program adds above it is its
  *own cost* ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Block**: the frames the engine processes in one callback, a period's worth.
- **Channel**: a strip of the mixer. Each track owns one; a multi-output instrument has child
  channels under its track ([ADR-0008](adr/0008-the-project-model.md)).
- **Collector**: the thread that frees what the real-time thread let go, so that it never frees
  memory itself.
- **Comping**: building one take from the best parts of several.
- **Compiled graph**: the order of the engine's nodes, their buffers and their compensating delays,
  built off the real-time thread and swapped in at a block's boundary
  ([ADR-0007](adr/0007-the-audio-engine.md)).
- **Creation bench**: five musical tasks, from a beat to stems for a game, timed in Magnolia and in
  its rivals: the waiting on the machine and the human time
  ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **DAWbench's method**: instances of one plugin, or an instrument's voices, added until the first
  xrun or missed deadline; the count measures a DAW's processing power.
- **Deadline**: the share of a period past which a plugin's answer is not awaited and its block is
  silent ([ADR-0003](adr/0003-every-plugin-out-of-the-engines-process.md)).
- **Drop-frame**: a timecode at 29.97 or 59.94 frames a second that skips frame labels, two or
  four at the start of each minute but every tenth, to stay in step with the clock.
- **Duplex**: input and output of a period handled in one callback.
- **Floor**: an absolute figure of the quality bar, which gates the milestone that measures it
  ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Foreign surface**: a surface drawn by another process, shown in an element of Magnolia's window,
  as a plugin's GUI will be.
- **Futex ping-pong**: two processes that wake each other in turn through a futex; the least that
  any bridge between processes pays for a round trip.
- **Gesture**: one action of the user that changes the project, such as a note drawn or a clip
  moved; one transaction in the project's file.
- **Grid**: a way of measuring time, whose positions each lie at an exact moment: samples, time
  units, ticks or timecode frames ([ADR-0018](adr/0018-the-time-model.md)).
- **Group commit**: gestures that arrive during a sync to disk, written together at the next one.
- **Label**: a timecode as a clock shows it, hours, minutes, seconds and frames, such as
  01:00:00:00; drop-frame skips some labels.
- **Lead**: a margin of the quality bar over the best rival, measured; missed, it is debt, never a
  gate ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Light sensor**: a sensor on the screen that times when an image appears, so that programs are
  compared from outside, input to photon.
- **Linked instance**: one placement of a pattern; an edit in one shows in all, except its own
  offset, length, loop, transposition and velocity scale.
- **MCP**: the Model Context Protocol, through which agents drive Magnolia.
- **Moment**: the exact instant of a position, through which every conversion of time goes
  ([ADR-0018](adr/0018-the-time-model.md)).
- **Null**: two renders that cancel exactly when one is subtracted from the other.
- **OSARA**: an extension that makes REAPER usable with a screen reader; Magnolia's reference for
  accessibility.
- **Own cost**: what a program costs above a blank window: memory, start, processor, GPU power and
  a frame's time.
- **Pattern**: a reusable block of notes and automation, over one or several instrument tracks.
- **Pattern lane**: the lane above the arrangement where patterns are painted.
- **Period**: the number of frames the audio device asks for at each callback, from 32 to 128 here.
- **Place, read**: the two ways to convert a position to another grid: placing gives the first point
  at or after its moment, reading the last point at or before it
  ([ADR-0018](adr/0018-the-time-model.md)).
- **Playable, fully loaded**: the two moments of opening a project: it plays, its plugins still
  loading as silent slots; then every plugin is loaded.
- **Playlist**: in Magnolia, as in Pro Tools, one of a track's alternate sets of clips. FL Studio's
  playlist is Magnolia's arrangement.
- **Plugin host**: a process apart from the engine that runs third-party plugins, native or under
  Wine.
- **Prefix**: a folder holding one Windows installation for Wine, its programs and its registry.
- **Primary modifier**: the key that shortcuts use, Ctrl on Linux and Windows, Cmd on macOS.
- **Reference project**: a session fixed in M1.3 for each tier, on which the sound's budgets are
  measured, never tuned for afterwards ([`reference-projects.md`](reference-projects.md)).
- **Render-ahead**: rendering tracks without live input before the playhead reaches them, on worker
  threads, as REAPER does.
- **Rest**: the transport stopped, nothing armed or monitored, plugin GUIs closed. Magnolia then
  draws nothing, plugin hosts receive no calls, and nothing of Magnolia's wakes outside the audio
  device's callback, while the device stays open
  ([ADR-0010](adr/0010-the-quality-bar.md)).
- **rtkit**: a system service that grants real-time priority to a program's threads.
- **Scan-out**: the time a display takes to send an image's lines to the panel, from top to bottom.
- **Screen of reference**: a screen fixed before the work, on which uwui and its rivals are
  measured.
- **Snapshot of a plugin's state**: its state saved when it reports a change, or read at an
  interval when it never does.
- **Song start**: the time position of tick 0 in a tempo map.
- **Stems**: a mix exported as several files, one per group of tracks, that sum to the mix.
- **Take lane**: one recorded take of a track, kept beside the others for comping.
- **Tempo map**: where musical time lies on the absolute timeline: tempo changes at ticks, meter
  changes at bars, from the song start.
- **Test of signature**: uwui's test that an application on uwui is not recognised by its look
  alone.
- **Test session**: Stage 1's description of a session, read only by tests and the command line.
- **Tick**: the unit of musical time, 1/4,838,400 of a quarter note
  ([ADR-0018](adr/0018-the-time-model.md)).
- **Tier**: a class of machine with its own budgets; the high tier is the owner's desktop, the
  lowest tier a two-core laptop.
- **Time unit**: the unit of absolute time, 1/903,168,000,000 of a second; every standard sample and
  every frame is a whole number of them ([ADR-0018](adr/0018-the-time-model.md)).
- **Timecode clock**: where timecode frames lie on the absolute timeline: a frame rate and a start
  label at a time position.
- **WAL**: SQLite's write-ahead log, which makes each transaction whole or absent after a crash.
- **Winelib**: a program built for Linux that also calls Windows' functions through Wine.
- **Wine bridge**: the plugin host built for Windows, run under the system's Wine
  ([ADR-0004](adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
- **Wine test set**: the Windows plugins on which the bridge is judged: the owner's, and the free
  VST3 and CLAP plugins the owner installs; its results are public.
- **Xrun**: a period that the program did not fill in time, heard as a click or a gap.
- **Xwayland**: the X11 server that runs X11 programs inside a Wayland session.
