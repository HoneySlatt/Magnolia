# Glossary

The terms Magnolia coins, and the audio and system terms its documents use without explaining.

- **Agent bench**: fixed tasks given through MCP to the same agents, in the same words, with
  Magnolia and with its rivals' servers; success, tries, tokens and time measured
  ([ADR-0016](adr/0016-agents-on-the-shape-of-uwuis-agent-core.md)).
- **ARA**: an interface through which a plugin such as Melodyne reads a clip's whole audio and
  edits it in place.
- **Arena**: the store where the engine's nodes live, apart from any compiled graph, so that they
  survive a swap.
- **Armed track**: a track that records: from each play, its capture tap copies its capture
  channels into its takes ([`ARCHITECTURE.md`](ARCHITECTURE.md#recording)).
- **Arrangement**: the timeline of a song's tracks and clips; FL Studio calls it the *playlist*.
- **Audible region**: the timeline samples a play makes heard, from the position asked for up to
  the stop; a timeline source plays only the events inside it
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-transport)).
- **Audio player**: the node that plays an audio track's stream on the real-time thread, at its own
  position on the timeline ([`ARCHITECTURE.md`](ARCHITECTURE.md#audio-clips-from-disk)).
- **BBT**: a musical position in bars, beats and ticks.
- **Best rival**: on a criterion, the rival that does best, measured; a lead is a margin over it
  ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **bext**: the Broadcast WAV chunk (EBU Tech 3285) that names a file's origin and holds its
  time reference; a take's places it on the timeline and marks it as Magnolia's.
- **Blank window**: a window that only clears and presents. What a program adds above it is its
  *own cost* ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Block**: the frames the engine processes in one callback, a period's worth. A stream's block is
  another thing: 4,096 frames of a track's clips, mixed, what one slot holds.
- **Buffer pool**: the one set of channel buffers the engine's processor works in, an input region and
  an output region, each channel a stride long; compiled graphs name its channels by index
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#memory-the-real-time-thread-touches)).
- **Capture overrun**: a frame the capture tap dropped because its track's ring had no room, the
  recorder having fallen behind; its take holds a zero there, so the rest stays in place.
- **Capture tap**: the processor's part that copies each armed track's capture channels into the
  track's rings, at each period's start, before the graph, placed by the round trip
  ([ADR-0032](adr/0032-recording-by-a-capture-tap-of-the-processor.md)).
- **Channel**: a strip of the mixer. Each track owns one; a multi-output instrument has child
  channels under its track ([ADR-0008](adr/0008-the-project-model.md)).
- **Chase**: at a play's start, each MIDI sequence first sends the last pitch bend and controller
  values set before it, so that the music starts as it would sound there
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#timeline-sources)).
- **Collector**: the thread that frees what the real-time thread let go, so that it never frees
  memory itself.
- **Comping**: building one take from the best parts of several.
- **Compiled graph**: the phases of a swap, each the order of the engine's nodes and what each of
  their channels sums, built off the real-time thread and swapped in at a period's start; from
  M1.4 it holds the delays that compensate plugins' latencies too
  ([ADR-0007](adr/0007-the-audio-engine.md)).
- **Controller**: the engine's non-real-time half: it edits the graph's description, compiles each
  swap and sends it, and sends events ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-engine)).
- **Creation bench**: five musical tasks, from a beat to stems for a game, timed in Magnolia and in
  its rivals: the waiting on the machine and the human time
  ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Crossfade**: a swap whose one fading phase runs the union of the old and new graphs, the paths
  removed fading out while the paths added fade in, one fade on each path that changes
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#a-swap)).
- **Cue**: where a play starts: a timeline sample, and the generation of the disk streams it reads,
  which a locate gives ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-transport)).
- **DAWbench's method**: instances of one plugin, or an instrument's voices, added until the first
  xrun or missed deadline; the count measures a DAW's processing power.
- **Deadline**: the share of a period past which a plugin's answer is not awaited and its block is
  silent ([ADR-0003](adr/0003-every-plugin-out-of-the-engines-process.md)).
- **Dip**: a swap that fades the old graph's removed edges out, then the new graph's added edges in,
  one phase each, when the union of both graphs would hold a cycle or too many nodes.
- **Disk reader**: the thread that reads audio clips from their files ahead of the playhead and
  fills each audio track's stream ([ADR-0026](adr/0026-one-stream-per-audio-track.md)).
- **Drop-frame**: a timecode at 29.97 or 59.94 frames a second that skips frame labels, two or
  four at the start of each minute but every tenth, to stay in step with the clock.
- **Dummy driver**: JACK2's driver that runs periods on a timer, without a sound card; the JACK
  backend's tests run on it.
- **Duplex**: input and output of a period handled in one callback.
- **Fade**: a raised cosine over 5 ms, by which a swap's edges come in or go out; on one edge, a
  fade-in and its fade-out sum to one at every sample.
- **Floor**: an absolute figure of the quality bar, which gates the milestone that measures it
  ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Foreign surface**: a surface drawn by another process, shown in an element of Magnolia's window,
  as a plugin's GUI will be.
- **Freewheel**: a JACK server's mode that runs periods as fast as its clients finish them, without
  the device; the JACK tests play in it, faster than real time.
- **Futex ping-pong**: two processes that wake each other in turn through a futex; the least that
  any bridge between processes pays for a round trip.
- **Frame time**: the server's count of frames since it started, 32 bits, which wraps after about
  25 hours at 48 kHz; each period's first frame has one.
- **Gap**: a period whose frame time does not follow on from the last one's, as when the server
  skipped the client's cycles; the JACK backend counts them
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#what-the-callback-counts)). A take's gap is another thing:
  frames the capture tap dropped, which the recorder writes as zeros.
- **Generation**: how many nodes have occupied an arena slot before; a node's id holds it, so that
  an old id never reaches the node that reuses its slot. A disk stream's generation is another
  thing: the number the engine gives each locate, which a stream's blocks were filled for; an audio
  player reads only its play's ([`ARCHITECTURE.md`](ARCHITECTURE.md#audio-clips-from-disk)).
- **Gesture**: one action of the user that changes the project, such as a note drawn or a clip
  moved; one transaction in the project's file.
- **Grid**: a way of measuring time, whose positions each lie at an exact moment: samples, time
  units, ticks or timecode frames ([ADR-0018](adr/0018-the-time-model.md)).
- **Group commit**: gestures that arrive during a sync to disk, written together at the next one.
- **Held event**: an event the controller keeps until the processor has applied the swap that
  brings its node, so that it never reaches the processor before its node
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#events)).
- **Heartbeat**: the count of periods the processor has run, which stops when the backend stops
  calling it.
- **Input monitoring**: hearing a track's input live through its plugins while it plays or records;
  Magnolia never delays it to line it up with the timeline.
- **Label**: a timecode as a clock shows it, hours, minutes, seconds and frames, such as
  01:00:00:00; drop-frame skips some labels.
- **Lead**: a margin of the quality bar over the best rival, measured; missed, it is debt, never a
  gate ([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
- **Light sensor**: a sensor on the screen that times when an image appears, so that programs are
  compared from outside, input to photon.
- **Linked instance**: one placement of a pattern; an edit in one shows in all, except its own
  offset, length, loop, transposition and velocity scale.
- **Locate**: the disk reader's start of a new generation from a timeline sample, which it fills
  every stream from; it gives the cue a play from there takes.
- **Loopback**: a cable from an output of the audio interface to one of its inputs, by which a
  round trip is measured.
- **Low-water mark**: the fewest frames an audio player found ready ahead of it since its play
  started.
- **Mark**: what a capture tap sends the recorder beside a track's frames: a take's start, a gap, a
  take's end, each at its place among the frames ([`ARCHITECTURE.md`](ARCHITECTURE.md#recording)).
- **MCP**: the Model Context Protocol, through which agents drive Magnolia.
- **Memory budget**: the most memory a test session may ask for, 1 GiB, estimated from its file
  before anything large is allocated ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-memory-budget)).
- **Moment**: the exact instant of a position, through which every conversion of time goes
  ([ADR-0018](adr/0018-the-time-model.md)).
- **Null**: two renders that cancel exactly when one is subtracted from the other.
- **OSARA**: an extension that makes REAPER usable with a screen reader; Magnolia's reference for
  accessibility.
- **Own overrun**: a period that Magnolia's callback itself made late: it started before the
  period's deadline and ended after it, or its work took longer than a period
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#what-the-callback-counts)).
- **Own cost**: what a program costs above a blank window: memory, start, processor, GPU power and
  a frame's time.
- **Pattern**: a reusable block of notes and automation, over one or several instrument tracks.
- **Pattern lane**: the lane above the arrangement where patterns are painted.
- **Period**: the number of frames the audio device asks for at each callback, from 32 to 128 here.
- **Phase**: one stage of a swap: which nodes run, in which order, and the gain of each edge; every
  phase but the last lasts one fade, the last is steady.
- **Place, read**: the two ways to convert a position to another grid: placing gives the first point
  at or after its moment, reading the last point at or before it
  ([ADR-0018](adr/0018-the-time-model.md)).
- **Playable, fully loaded**: the two moments of opening a project: it plays, its plugins still
  loading as silent slots; then every plugin is loaded.
- **Playlist**: in Magnolia, as in Pro Tools, one of a track's alternate sets of clips. FL Studio's
  playlist is Magnolia's arrangement.
- **Plugin host**: a process apart from the engine that runs third-party plugins, native or under
  Wine.
- **Poisoned**: the processor's state after a panic on the real-time path: it writes silence to every
  output from then on, and says so ([ADR-0022](adr/0022-a-panic-on-the-real-time-path-gives-silence.md)).
  After the graph's panic alone, its transport still follows its commands, so that a stop still
  ends the takes under way ([`ARCHITECTURE.md`](ARCHITECTURE.md#a-period)).
- **Pre-roll**: the samples the transport runs before the position a play asked for, as many as the
  graph's largest compensation, so that every path reaches the output on that position together
  ([ADR-0025](adr/0025-delay-compensation-by-reading-ahead.md)).
- **Prefix**: a folder holding one Windows installation for Wine, its programs and its registry.
- **Primary modifier**: the key that shortcuts use, Ctrl on Linux and Windows, Cmd on macOS.
- **Primed**: a cue is primed when every stream holds what a play from it reads first: half a
  second from its sample, or half the read-ahead if shorter, or up to the track's last clip's end.
  A play of the cue waits until then ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-transport)).
- **Processor**: the engine's real-time half, which a backend calls once a period; it applies swaps,
  runs the nodes and fills the device's buffers ([`ARCHITECTURE.md`](ARCHITECTURE.md#a-period)).
- **pw-jack**: PipeWire's command that runs a program with PipeWire's libjack first on its library
  path, so that a JACK client plays on PipeWire.
- **Read-ahead**: how many frames ahead of its player the disk reader keeps each stream, 10 seconds
  by default; a player that holds less than half of it asks for a refill.
- **Real-time rights**: what lets a thread run at real-time priority: rtkit, or the user's
  real-time priority limit (`RLIMIT_RTPRIO`); without them the audio thread shares the processor
  with every other program.
- **Recorder**: the thread that writes armed tracks' takes from their rings, with the sync threads
  that make them durable ([`ARCHITECTURE.md`](ARCHITECTURE.md#recording)).
- **Recovery**: the repair, under the recording folder's lock, of the takes a crash left unfinished
  ([ADR-0030](adr/0030-takes-in-wav-of-unknown-length-repaired-on-recovery.md)).
- **Reference project**: a session fixed in M1.3 for each tier, on which the sound's budgets are
  measured, never tuned for afterwards ([`reference-projects.md`](reference-projects.md)).
- **Refill**: a pass of the disk reader that fills some stream.
- **Render-ahead**: rendering tracks without live input before the playhead reaches them, on worker
  threads, as REAPER does.
- **Rest**: the transport stopped, nothing armed or monitored, plugin GUIs closed. Magnolia then
  draws nothing, plugin hosts receive no calls, and nothing of Magnolia's wakes outside the audio
  device's callback, while the device stays open
  ([ADR-0010](adr/0010-the-quality-bar.md)).
- **RF64**: the WAV variant whose sizes are 64 bits wide, in a `ds64` chunk, for files past 4 GiB.
- **Round trip**: how late the input hears what the output plays: the device's playback and capture
  latencies, plus the recorder's extra latency; a take is placed by it
  ([ADR-0031](adr/0031-the-round-trip-reported-and-takes-placed-by-it.md)).
- **rtkit**: a system service that grants real-time priority to a program's threads.
- **Scan-out**: the time a display takes to send an image's lines to the panel, from top to bottom.
- **Screen of reference**: a screen fixed before the work, on which uwui and its rivals are
  measured.
- **Sleep**: what a node returns when it needs no call while its inputs stay silent and no event
  comes; the processor then skips it and writes silence in its place
  ([ADR-0021](adr/0021-the-node-contract.md)).
- **Snapshot of a plugin's state**: its state saved when it reports a change, or read at an
  interval when it never does.
- **Song start**: the time position of tick 0 in a tempo map.
- **Steady phase**: a swap's last phase, which lasts until the next swap; when it begins, the nodes
  the swap dropped leave the arena.
- **Stems**: a mix exported as several files, one per group of tracks, that sum to the mix.
- **Stream**: an audio track's ring of slots, each holding one block of its clips mixed and a word
  saying which block of which generation it holds; the disk reader fills it, the track's audio
  player plays it ([ADR-0026](adr/0026-one-stream-per-audio-track.md)).
- **Stride**: the length of every channel buffer in the buffer pool: the engine's largest block,
  rounded up to 16 floats.
- **Sub-block**: a stretch of a period that the processor runs at once, never longer than the
  engine's largest block; it cuts a period there and at the end of each phase.
- **Swap**: the change from one compiled graph to the next, applied whole at a period's start and
  played through its phases without a click.
- **Take**: what an armed track records over one play: a WAV file whose first frame is the frame
  captured a round trip after the output played the play's first timeline sample.
- **Take lane**: one recorded take of a track, kept beside the others for comping.
- **Tempo map**: where musical time lies on the absolute timeline: tempo changes at ticks, meter
  changes at bars, from the song start.
- **Test of signature**: uwui's test that an application on uwui is not recognised by its look
  alone.
- **Test session**: Stage 1's description of a session, a JSON file beside its WAV and MIDI files,
  read only by tests and the command line
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-test-session)).
- **Tick**: the unit of musical time, 1/4,838,400 of a quarter note
  ([ADR-0018](adr/0018-the-time-model.md)).
- **Tier**: a class of machine with its own budgets; the high tier is the owner's desktop, the
  lowest tier a two-core laptop.
- **Time unit**: the unit of absolute time, 1/903,168,000,000 of a second; every standard sample and
  every frame is a whole number of them ([ADR-0018](adr/0018-the-time-model.md)).
- **Timecode clock**: where timecode frames lie on the absolute timeline: a frame rate and a start
  label at a time position.
- **Timeline source**: what gives a node events from the timeline while the transport rolls: a MIDI
  sequence, and from M1.6 an automation lane
  ([ADR-0027](adr/0027-timeline-events-from-sources-on-nodes.md)).
- **Transport**: what plays and stops the timeline. Its clock is the timeline sample the output
  plays; the engine's own clock counts every sample, rolling or not
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#the-transport)).
- **Underrun**: a frame an audio player needed that its stream did not hold ready; it plays silence
  there, never waits, and counts it in `EngineStats::disk_underruns`.
- **Window**: the engine samples whose capture a take holds: from a round trip after the output
  plays the play's first timeline sample, for the length of its audible region
  ([`ARCHITECTURE.md`](ARCHITECTURE.md#recording)).
- **WAL**: SQLite's write-ahead log, which makes each transaction whole or absent after a crash.
- **Winelib**: a program built for Linux that also calls Windows' functions through Wine.
- **Wine bridge**: the plugin host built for Windows, run under the system's Wine
  ([ADR-0004](adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
- **Wine test set**: the Windows plugins on which the bridge is judged: the owner's, and the free
  VST3 and CLAP plugins the owner installs; its results are public.
- **Xrun**: a period that the program did not fill in time, heard as a click or a gap.
- **Xwayland**: the X11 server that runs X11 programs inside a Wayland session.
