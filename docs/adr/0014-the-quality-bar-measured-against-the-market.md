# 0014. The quality bar measured against the market

- Status: Accepted; amends [0010](0010-the-quality-bar.md) (rivals measured, floors and leads, runs
  of eight hours, new families of budgets)
- Date: 2026-11-10

## Context

[ADR-0010](0010-the-quality-bar.md)'s bar holds absolute budgets. Only one compares Magnolia with
another program: the engine's time per period against REAPER's. On 2026-11-10 the owner asked that
Magnolia be the best DAW on the market, and that this be measured, as SlimeEngine measures its
engine ([SlimeEngine's ADR-0215](https://github.com/HoneySlatt/SlimeEngine/blob/main/docs/adr/0215-the-engine-measured-against-the-market.md))
and uwui its applications
([SlimeEngine's ADR-0213](https://github.com/HoneySlatt/SlimeEngine/blob/main/docs/adr/0213-uwuis-reach-its-lead-its-themes-and-agents.md)).

What the field shows, as found on 2026-11-10
([survey](../survey.md#the-market-measured-2026-11-10)):

- REAPER and Ardour run plugins inside their own process by default. Bitwig Studio runs its audio
  engine in a process of its own and can isolate plugins. A call into a plugin in another process
  costs at least two context switches ([ADR-0003](0003-every-plugin-out-of-the-engines-process.md)).
- No DAW maker ships a server for agents. Community servers drive REAPER, Ableton Live, Bitwig,
  FL Studio, Logic, Pro Tools and Ardour through their scripting interfaces.
- REAPER with OSARA is the DAW that blind musicians use most. OSARA runs on Windows and macOS only.
  Ardour with Orca has been reported unusable.
- No figure is published for a DAW on a two-core laptop.

A review of the first figures proposed, made before this ADR was written, found figures that no
work could meet, and tests that could not prove what they claimed:

- **The cost of isolation.** Both programs run the same plugin, so its own work costs the same in
  each; only the call differs. A lead over a host that runs plugins inside its own process is out of
  reach for one that isolates them.
- **Near-zero figures.** A margin over a figure near zero is out of reach: a rival's added latency,
  or the GPU power of a rival that draws on the processor.
- **Short runs.** One hour without an xrun shows only that xruns are rarer than three an hour, at
  95 % confidence. Eight hours show fewer than 0.4 an hour.
- **Crash rates.** Showing 99.99 % of hours without a crash takes about 30,000 hours with none.
- **Small panels.** Blind tests among a few listeners who know Magnolia prove nothing. One run of
  each agent task measures chance.
- **Virtual machines.** Figures from a virtual machine do not compare with the bare machine's.

## Decision

The owner's decisions of 2026-11-10, and their reasons. The
[ROADMAP's quality bar](../ROADMAP.md#quality-bar) holds each figure as it stands: a figure changed
later changes there, with an ADR that says why.

**Floors and leads.** Each family of budgets has a floor and a lead:

- A floor is an absolute figure. It gates the milestone that measures it. ADR-0010's figures stay as
  floors unless this ADR raises them.
- A lead is a margin over the best rival, measured. A lead missed is debt, never a gate. Its figure
  is set once the rival is measured. The rival's version is frozen until the next stage audit,
  which measures it again.
- Where the hardware sets the limit, such as added latency, the floor alone is the bar.

**The rivals, and where they are measured.**

- The engine is measured on Linux against REAPER, Bitwig Studio and Ardour, on both tiers.
- Creation and the interface are also measured against FL Studio, Pro Tools, Ableton Live,
  Studio One and Cubase, in the owner's Windows virtual machine. Those figures are indicative,
  never a bar for time, power or frames.
- At the Windows stage, Windows installed on a disk of its own measures every rival.
- Each rival's figure names its version, its settings and its date. A measure that needs the
  owner's licenses or accounts is taken by the owner.

**Latency.**

- The floor: 64 frames at 48 kHz on the high tier's reference project, Windows plugins under Wine
  included, and 128 frames on the lowest tier. Each runs eight hours with no xrun and no missed
  deadline, so that a late plugin's silent block counts too.
- The lead: 32 frames on the high tier and 64 on the lowest. The lowest tier's lead is provisional
  until M1.3 has measured the laptop on mains power, its frequency governor and audio device named.
- On a reference project with a voice monitored live, the smallest period that Magnolia holds is
  never larger than the best rival's.
- No added latency, as ADR-0010 sets.

**Processing power, measured as DAWbench does.** Instances of one plugin, then an instrument's
voices, are added until the first xrun or missed deadline, at 32, 64 and 128 frames.

- The lead: 1.25 times the best rival that isolates plugins in the same way, such as Bitwig's
  isolated modes or REAPER's dedicated process.
- The floor: at least 0.9 times REAPER running the same plugin inside REAPER's own process, its
  default, at 64 and 128 frames. The figure at 32 frames is recorded.
- Both figures are revised on M1.6's prototype.

**The engine's efficiency follows the same rule.** On the reference project, Magnolia's processor
time per period is at most 1.1 times REAPER's: that is the floor. The lead is to be below REAPER's.
ADR-0010 held it to REAPER's time; if Magnolia may hold 10 % fewer plugins than REAPER, its time
per period cannot be held to REAPER's.

**The Wine bridge.**

- A named test set: the owner's Windows plugins, and the free Windows VST3 and CLAP plugins the
  owner installs. Each loads, plays, opens its GUI, keeps its state and survives a kill. Each one
  that works under yabridge works under Magnolia. The list of results is public.
- The lead is measured on a plugin that does nothing, at 32 and 64 frames, at the median and the
  99.9th percentile. Magnolia's cost above a futex ping-pong between two processes is at most half
  of yabridge's. Any bridge pays that ping-pong; it cannot be halved.
- The floor stays ADR-0010's: at least as good as yabridge.

**The sound.** Objective tests come first:

- resampling at least equal to the best of r8brain and SoX, on named figures: alias rejection,
  passband ripple, noise;
- a loudness meter that passes EBU Tech 3341's test signals;
- a limiter that lets no true-peak over through;
- dither on export when the bit depth is reduced.

Blind listening comes next, with at least 20 judges new to Magnolia. Its protocol and its pass rule
are written before the test: ABX, or MUSHRA with a margin of non-inferiority.

- Time-stretch is at least equal to élastique and Ableton's Complex Pro.
- The built-in essentials are preferred, or not told apart, against the plugins that the best DAWs
  bundle, for less processor per instance.

**The sound's safety.**

- A plugin's NaN, infinity or runaway level never reaches an output.
- A plugin that crashed is reloaded with its last state within a time set at M1.6, against
  Bitwig's.

**Plugin conformance.** Each CLAP and VST3 feature that the engine supports has a test plugin in CI.
Among them: a sidechain, multiple outputs, note expressions, a change of latency, sample-accurate
automation.

**Large sessions.** 1,000 tracks and 50,000 clips are opened, edited and played on the high tier,
and the time of an edit is measured. The figures are set at Stage 2.

**The creation bench.** Five tasks:

- a beat from nothing to its export;
- a vocal comp;
- a mix to a loudness target;
- music in step with a video;
- a game's loop exported as stems.

Each task is measured against FL Studio, Pro Tools, Ableton Live and REAPER, and judged against its
best rival:

- 4 times less waiting on what Magnolia controls: its start, opening a project, scans, peaks,
  imports;
- renders at least as fast as the best rival;
- 1.5 times less human time, over variants of each task done in a balanced order across several
  sessions. The figure is claimed only beyond their spread, with the bias of habit noted beside.

Each task runs from the stage that brings what it needs: comping from Stage 4, video from the film
stage.

**The interface.** uwui's lead applies to Magnolia: its own cost above a blank window is at most a
third of the best rival's. That cost covers memory (all processes counted), start, processor, GPU
power and a frame's time.

- Each rival is compared at its own frame rate, Magnolia capped to it.
- Where a rival's figure is under the noise of the measure, Magnolia matches the blank window.
- Frame time comes from Magnolia's own trace: 99.9 % of frames within the frame, none over one and
  a half frames. Stalls of the compositor are counted apart.
- Meters, the playhead and input to screen are timed by a light sensor on the screen, recorded with
  the audio. The budget allows for scan-out and the panel's response.
- The look is judged on written criteria, and blind by at least 20 judges, against Bitwig and
  Ableton Live, and preferred. The look itself is decided at Stage 3.
- Rivals measured only in the virtual machine give indicative figures until the Windows stage.

**Start, memory and loading.**

- An empty project: Magnolia's own cost at start and in memory is at most a third of the best
  rival's, from Stage 3, when Magnolia has a window. M1.8 measures the engine's start beside the
  rivals' for reference.
- The reference project is *playable* within half the best rival's time to play it, and *fully
  loaded* never later than it.
- The memory of each plugin host is measured apart, its budget set at M1.6.
- ADR-0010's absolute figures stay as floors.

**The lowest tier.**

- Its reference project is a song with vocals over a beat: 16 audio tracks streamed from disk, 8
  instrument tracks, and 16 plugins, 4 of them Windows plugins under Wine.
- It is defined and its plugins named at M1.3: Kontakt with a small library, for example. It is
  provisional until the laptop is measured.
- It fits within ADR-0010's 2.5 GB, measured at M1.9.
- The lead: 1.25 times what REAPER holds on the same laptop, set once M1.9 has measured both.

**Reliability.**

- The goal: under one crash per 10,000 hours of use for the engine, and under one per 1,000 for the
  interface, uwui's own bar. Plugins' crashes are counted apart.
- The rate observed is published with its upper bound at 95 %. It is not claimed before the hours
  show it: the engine's figure takes about 30,000 hours without a crash.
- Hours are counted on the machine and sent only by the user's choice: nothing ticked in advance,
  "no" as easy as "yes". Hours of CI are counted apart.
- No xrun caused by Magnolia during a take. An xrun is Magnolia's when its own callback overran.
- A test in CI kills the engine and the interface at random, and finds nothing lost: every gesture
  the engine acknowledged, and every sample written before the kill.

**Compiling.** SlimeEngine's figures, on the high tier, measured at each milestone. No rival.

- 2 s from a change to the engine relaunched and ready on a session without plugins, in a debug
  build.
- 10 s after a change to a low-level crate, for the Linux target. The Windows plugin host's build
  is measured apart.
- 3 min from nothing. This figure is set again when uwui joins.
- 10 min for the release build.
- From Stage 3, a change to the interface is seen within 2 s.

**Accessibility.**

- A checklist drawn from OSARA: every state, position, parameter and level spoken, and the whole
  program by the keyboard. It holds with Orca on Linux, checked through AT-SPI in CI from Stage 3,
  and with NVDA by hand from the Windows stage.
- From Stage 3, sessions with blind volunteers, with their consent, scored on the success of the
  creation tasks, video excepted.
- At the Windows stage, success and time are measured against REAPER with OSARA under NVDA:
  Magnolia's tasks succeed at least as often and as fast.

Agents and their bench are [ADR-0016](0016-agents-on-the-shape-of-uwuis-agent-core.md)'s.

## Alternatives considered

- **ADR-0010's absolute budgets alone**: they never say whether Magnolia beats anyone.
- **Every lead as a gate**: milestones that no work could close, and a pass turned into a failure
  by a rival's update.
- **32 frames as the high tier's floor, 64 as the lowest's**: about 2 ms gained on a Scarlett
  (6.5 ms down to 4.5), against ADR-0010's reasons. Stage 1 might never close, the laptop least of
  all.
- **1.25 times REAPER running plugins inside REAPER's own process**: out of reach at equal work per
  plugin.
- **99.99 % of hours without a crash**: unprovable in a project's lifetime, and ten times uwui's
  own bar.
- **The bridge's whole round trip at half of yabridge's**: the ping-pong that every bridge pays
  cannot be halved.
- **Blind tests among the owner's close ones**: they know Magnolia, and five judges prove only a
  unanimous preference.
- **Figures from the virtual machine as a bar**: the GPU, disk and audio interface are not the bare
  machine's.
- **Battery life and the package's size against the rivals**: not what makes a professional choose
  a DAW. Left out for now.

## Consequences

- The ROADMAP's quality bar becomes two tables: floors, and leads with their rivals.
- Stage 1's exit runs eight hours instead of one.
- Stage 1 measures more:
  - M1.3: the laptop, and the lowest tier's reference project defined, its plugins named;
  - M1.6: power against isolated rivals, the time of a reload, a plugin host's memory, the guard
    against NaN, and the conformance plugins;
  - M1.7: the bridge's cost above a ping-pong;
  - M1.8: the engine's start beside the rivals';
  - M1.9: REAPER, Bitwig Studio and Ardour on both tiers, and the lowest tier's memory.
- Blind tests need about 20 judges new to Magnolia, found at Stage 3 and Stage 4.
- The light sensor comes with SlimeEngine's Stage 6e, and the owner buys it.
- A rival's update can turn a lead met into debt at the next stage audit; the floors never move with
  them.
