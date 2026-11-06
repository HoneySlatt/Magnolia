# What Magnolia asks of uwui

Magnolia's interface will be written on uwui, the owner's UI framework
([ADR-0006](adr/0006-the-interface-on-uwui-after-the-engine-and-the-project.md)). Magnolia is the DAW
among the programs of uwui's first stage (SlimeEngine's
[ADR-0203](https://github.com/HoneySlatt/SlimeEngine/blob/main/docs/adr/0203-pilots-replaced-pluto-and-a-daw.md)).
The "Magnolia" that ADR-0198 and ADR-0203 abandon is an earlier, unrelated media application, now
`Magnolia-media`; the DAW took the name afterwards.

SlimeEngine's milestone M6d.1b3b chooses how an application's state changes its interface once three
lists exist: Opium's, Pluto's and this one. This file is Magnolia's list. It comes from the owner's
interview of 2026-11-06, before any code. Nothing here is measured yet.

## How to read it

- uwui takes an application's need only if it is **general**: several kinds of applications need
  it. A need that serves only Magnolia, or only one benchmark, stays in Magnolia.
- **General needs** are what Magnolia asks uwui to hold. **Magnolia's own** are what Magnolia builds
  itself on uwui's general pieces.
- **Mark**: *indispensable* when Magnolia cannot meet the owner's requirements or its
  [quality bar](ROADMAP.md#quality-bar) without it; *wished* otherwise. Each mark names the stage of
  Magnolia's [ROADMAP](ROADMAP.md#4-stages) that first needs it: Stage 3 is the interface; the
  plugin GUIs' stage and the film stage come after Stage 4.
- **In uwui's requirements**: whether SlimeEngine's ADR-0190 and uwui's ROADMAP already hold the
  need, as read on 2026-11-06. *Yes* means they hold it. *Precision* means they hold it in general
  terms and this list says exactly what is needed. *New* means they do not hold it.

## Magnolia's state, for M6d.1b3b

These are the changes Magnolia makes to its interface, from the most frequent to the rarest. In
every case the interface thread never waits on the audio engine, and nothing is drawn once the
change is over.

- **Every frame while the transport plays or a signal passes.** The playhead moves. Up to 128
  stereo meters move: peak, RMS and gain reduction of every channel. A knob or a fader follows its
  automation. A clip being recorded grows. These are a few drawn properties each. Their values come
  from the audio thread, not from the interface's code. No view should be built again for them, no
  style computed, no layout run, nothing allocated. The audio thread produces several values
  between two frames: a meter must show the highest since the last frame, not the newest, or peaks
  are lost; a scope or a growing waveform needs the short history since the last frame.
- **Each frame is drawn for the moment it is seen.** The sound heard is late by the output's latency
  (a few milliseconds), and the frame is late by its own presentation time. Meters and the playhead
  must show the sound being heard, within one frame. The interface needs to know when the next frame
  will be presented, to pick the right value.
- **While the user drags.** Notes, clips, fades and automation points move under the pointer at the
  display's rate. Only the dragged items and what depends on them change; the rest of a timeline of
  thousands of clips stays as it is.
- **While the user zooms and scrolls.** The timeline zooms from single samples to hours: nine orders
  of magnitude. Positions on it exceed what a 32-bit float holds (a three-hour session at 48 kHz is
  518 million samples). Only the clips and notes in view exist as elements. A waveform is drawn from
  peak data at the level of detail the zoom needs. Zooming must not rebuild the views of clips that
  stay in view.
- **In bursts, from other threads.** Waveform peaks computed while a file imports. Plugin scans
  finding hundreds of plugins. A stem separation's progress. Recording's new data. Each is gathered
  into the next frame.
- **By the user, as edit commands.** Every edit is a command on the project
  ([ADR-0009](adr/0009-the-project-in-sqlite-nothing-lost.md)): a note added, a clip split, a
  channel routed. The project model changes, then the interface follows it, the selection and the
  view kept. Undo goes through the same path.
- **Rarely.** The theme, the density or the language changes; a project opens or closes.

The first two cases are those most likely to separate the four state models that M6d.1b compares.
They are the same need as Opium's moving wave, at a larger scale: hundreds of values each frame,
fed from a real-time thread.

## General needs

| Need | Magnolia's use | Mark | In uwui's requirements |
|---|---|---|---|
| Values that change every frame, bound to drawn properties alone: written by another thread without a lock, read when the element is painted, without running the application's code, styles or layout, and without allocating | The playhead, knobs following automation | Indispensable: Stage 3 | Precision: the state model; Opium asks the same for its wave |
| Such values accumulated between two frames by their writer: the highest since the last read, or a short history in a ring buffer | Peak meters, scopes, a recording's waveform as it grows; elsewhere, a system monitor's graphs | Indispensable: Stage 3 | New |
| The predicted presentation time of the next frame, per window and per output, given to the application before it draws | Meters and playhead in step with the sound heard; video in step with audio | Indispensable: Stage 3 | New |
| Frames requested while something moves, and none once it stops, without a timer of the application's own | Frames while playing; none when stopped | Indispensable: Stage 3 | Yes: frames follow the display, none at rest |
| Drawing only what changed: the regions damaged in a frame, the rest of the window kept, so that a playhead at 240 Hz does not redraw a 4K window | A moving playhead over a still arrangement | Indispensable: Stage 3 | Precision: ADR-0190's groups drawn apart into cached images |
| A 2D view of a very large space: positions in 64-bit, scrolled and zoomed smoothly over nine orders of magnitude, its content virtualized along both axes, laid out relative to the view so that 32-bit drawing stays exact | The arrangement, the piano roll, automation lanes | Indispensable: Stage 3 | New: ADR-0190 has virtual lists, a map of tiles and charts, not a zoomable virtual space |
| A canvas that draws thousands of simple shapes per frame cheaply: lines, rectangles, filled paths, as instances on the GPU, with content cached between frames | Waveforms, notes, grids, automation curves, meters | Indispensable: Stage 3 | Precision: ADR-0190's canvas and the shader's view |
| Precise direct manipulation: pointer capture, drags with a threshold, relative pointer motion with the pointer locked or hidden (an endless drag on a knob), fine adjustment with a modifier, scrolling while dragging near an edge, high-resolution wheel and touchpad scrolling | Knobs, faders, notes, clips, fades | Indispensable: Stage 3 | Precision: ADR-0190's input, without the locked pointer |
| Commands with shortcuts that the user remaps, with chords, that work in the window whichever panel has the focus, including while an embedded foreign surface has it | Every edit, editing by the keyboard as in Pro Tools, transport keys while a plugin is focused | Indispensable: Stage 3; the foreign surface at the plugin GUIs' stage | Precision: ADR-0190's remapped shortcuts; the foreign surface is new |
| Raw key presses and releases with their timestamps, without key repeat, beside text input | The computer's keyboard played as a piano, its timing kept | Indispensable: Stage 3 | New |
| Docking: panels split, tabbed, resized, detached into windows on other monitors, the layout saved and restored per document | Arrangement, mixer, piano roll, browser, video on two screens | Indispensable: Stage 3 | Precision: ADR-0190's docking and several windows, without detaching nor layouts per document |
| Several windows on monitors of different scales and rates, each window paced by its own output | A 4K 240 Hz screen beside a 60 Hz one | Indispensable: Stage 3 | Precision: several windows; per-output pacing unstated |
| A window's handle exported to other processes (`xdg-foreign` on Wayland), so that their windows can be its children | Floating plugin windows kept above Magnolia's; the system's file dialogs through portals | Indispensable: Stage 3 | New |
| Accessibility of controls drawn by the application: a knob, a fader, a clip or a note exposed with its role, value, range, step and actions; values announced on request; a tree of thousands of items exposed lazily | Every control read by Orca and NVDA, levels read aloud, as REAPER with OSARA | Indispensable: Stage 3 | Precision: ADR-0190's WCAG 2.2 AA |
| Numbers that do not move as they change: tabular figures, crisp small text at every scale | Timecode, bars and beats, dB values | Indispensable: Stage 3 | Precision: Opium asks the same |
| A dense theme and a compact density | The owner's choice of a dark, dense look | Indispensable: Stage 3 | Yes: Pro theme, compact density |
| uwui's own threads few and of normal priority, its work bounded per frame, so that on two cores it never starves real-time threads | The lowest tier: the interface at 60 Hz beside the audio engine and plugin hosts | Indispensable: Stage 3 | New |
| Effects lightened on the lowest tier, under reduced motion and reduced transparency | The lowest tier; the owner's accessibility | Indispensable: Stage 3 | Yes: ADR-0190 |
| A table of tens of thousands of rows, sorted, filtered and searched as one types | The plugin and sample browsers | Indispensable: Stage 3 | Yes: ADR-0190's table |
| Undo and redo that an application may replace with its own model | Magnolia's undo lives in its project file ([ADR-0009](adr/0009-the-project-in-sqlite-nothing-lost.md)) | Indispensable: Stage 3 | Precision: ADR-0190 gives undo to applications; it must not impose its model |
| Drag and drop between applications both ways, with a preview drawn by the application during the drag | Samples dropped from a file manager; a clip dragged out as a file | Indispensable: Stage 3 | Precision: ADR-0190's drag and drop, without the application's preview |
| An element that shows a surface drawn by another process, composited with the tree, its input and focus forwarded and given back, its size negotiated, on Wayland (a subsurface or an imported buffer) and on Windows (a child window) | Plugin GUIs inside Magnolia's window ([ADR-0005](adr/0005-plugin-guis-floating-first-then-inside-the-window.md)) | Indispensable: the plugin GUIs' stage | Precision: ADR-0190 has the system's web view and an engine's scene in an element |
| Popups placed relative to an element and allowed past the window's edge | Plugin menus and tooltips, which Magnolia's compositor turns into its own popups | Indispensable: the plugin GUIs' stage | Yes: ADR-0190's menus and tooltips as windows of the system |
| Video in an element, each frame chosen by the application's clock rather than by a player's, seeking exact to the frame | The video track, ProRes and DNxHD included, in step with the music | Indispensable: the film stage | Precision: ADR-0190's video, optional; the external clock is new |
| Several fingers on several controls at once | Two faders moved together on a touch screen | Wished: after Stage 4 | Precision: ADR-0190's touch |

## Magnolia's own

These stay in Magnolia. They ask uwui only for the general needs above.

- The audio engine, its threads and its clock; the buffers that carry meters and positions to the
  interface.
- Plugin hosting, the plugin-host processes and the Wine bridge
  ([ADR-0003](adr/0003-every-plugin-out-of-the-engines-process.md),
  [ADR-0004](adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
- The small Wayland compositor that plugin GUIs draw into, and the private Xwayland for X11 and Wine
  plugins. uwui shows the surfaces; Magnolia serves them.
- The arrangement, the piano roll, the step sequencer, the mixer, the automation lanes and the
  meters' ballistics and loudness (EBU R128): components built on uwui's canvas and its zoomable
  view. None is a component of uwui's library.
- Peak files and their levels of detail.
- The mapping from the audio clock to presentation times: the engine gives the time each sample is
  heard; uwui gives the time each frame is seen.
- MIDI controllers and control surfaces.
- The project model, its journal and its undo.
- The generic editor of a plugin's parameters, and the spoken reports of levels and positions, in
  OSARA's manner.

## A screen of reference for Magnolia

uwui's ROADMAP measures each pilot on screens chosen before the work. The owner chose this one on
2026-11-06 ([ADR-0010](adr/0010-the-quality-bar.md)); entering it in uwui's ROADMAP is uwui's
decision.

- **The arrangement playing**: 128 tracks, 4,000 audio and MIDI clips with their waveforms, a
  session of three hours; the transport playing, the playhead moving, 128 stereo meters on a
  docked mixer; the view scrolled along the whole session, then zoomed from the whole session to
  single samples and back.
- **Rivals**: Bitwig Studio (drawn on the GPU since 5.2, native on Linux), REAPER and Ardour,
  measured from outside by uwui's `scripts/measure.py`. A script generates the same session for
  each: a DAWproject file for Bitwig, an `.rpp` file for REAPER, a session file for Ardour.
- **Criteria**: frames a second and their 99th percentile while playing and while zooming; the
  processor and the GPU's power while playing; the delay between the sound heard and the meter
  shown; nothing drawn and no wake-up of the interface once stopped.

## What Stage 3 needs first

Magnolia's interface starts once uwui gives at least these, in its first stage:

- the desktop host on Wayland, with several windows;
- values bound to drawn properties, and accumulated between frames;
- the presentation time of frames, and damaged regions;
- the zoomable virtual view, and the canvas of instanced shapes;
- pointer capture and the locked pointer;
- remappable shortcuts, docking, and a window's handle exported;
- the accessibility of drawn controls, tabular figures, and the Pro theme at compact density.

Drag and drop between applications and raw key presses can follow during Stage 3. The foreign
surface and video come with their own stages. The owner carries this set into uwui's plan.
