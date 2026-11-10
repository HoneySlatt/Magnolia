# 0015. The interface in a process of its own

- Status: Accepted; amends [0005](0005-plugin-guis-floating-first-then-inside-the-window.md) (the
  plugins' compositor a process of its own); its cost measured at Stage 3
- Date: 2026-11-10

## Context

Magnolia's interface comes at Stage 3, on uwui
([ADR-0006](0006-the-interface-on-uwui-after-the-engine-and-the-project.md)). Until then the engine
is driven by its command line and its MCP server. Every plugin already runs outside the engine's
process ([ADR-0003](0003-every-plugin-out-of-the-engines-process.md)).

If the interface ran in the engine's process, a crash of the interface would stop the sound. It
would also stop a take being recorded. An interface is large and changes often: of all of
Magnolia's code, it is the most likely to crash. The quality bar asks for no work lost after a
crash, and for no xrun caused by Magnolia during a take
([ADR-0014](0014-the-quality-bar-measured-against-the-market.md)).

Bitwig Studio runs its audio engine as an executable of its own, `BitwigAudioEngine`, beside the
program that draws its interface. A community report says that the engine survives a forced close
of the interface; Bitwig's documentation does not say so ([survey](../survey.md#the-market-measured-2026-11-10)). Ardour runs its engine and its interface
in one process, and has a build without an interface.

The plugin GUIs inside Magnolia's window are drawn by a small Wayland compositor of Magnolia's own,
which runs a private Xwayland for X11 and Wine plugins
([ADR-0005](0005-plugin-guis-floating-first-then-inside-the-window.md)). If an X server goes away,
Xlib's default handler ends its clients by default. A plugin under Wine may then take its plugin
host down with it. How Wine handles a lost X server is checked at M1.7.

## Decision

**The engine is a server.** It holds the project, the sound and the take, and it runs without a
window, a GPU or a network, as [`AGENTS.md`](../../AGENTS.md#architecture-rules) already asks. Its
clients are the interface, the command line and the MCP server. They change the project only
through its commands ([ADR-0009](0009-the-project-in-sqlite-nothing-lost.md)), so undo and the
project's file live in the engine alone.

**The interface is a process of its own.** When it crashes, the sound and the take go on. A new
interface process attaches to the running engine and finds the project as it was.

**Meters and positions go through shared memory.** The engine writes them without a lock. The
interface reads them when it draws a frame. No message is sent for each value.

**The plugins' compositor and its Xwayland are a process of their own.** A crash of the interface
does not take down a Wine plugin's X server, and so does not silence a monitored voice in the middle
of a take.

**A test kills the interface** while a take is recording, from Stage 3. The take keeps sounding and
keeps being written. From the stage of plugin GUIs inside the window, a Windows plugin's GUI is
open during the test.

## Alternatives considered

- **The interface in the engine's process**: one process fewer, no protocol between them. But any
  crash of the interface stops the sound and the take.
- **The engine as a library that the interface loads, with a separate engine only for recording**:
  two paths to keep, and the same crash still stops playback.
- **The plugins' compositor inside the interface's process**: simpler, but a crash of the interface
  can then end Wine plugins through their X server.

## Consequences

- The protocol between the engine and its clients is designed once, for the command line and MCP
  in Stage 1, and the interface uses it at Stage 3.
- The cost of the extra process is measured at Stage 3: memory, start, and the delay from a
  command to its result on screen. These count in the interface's own cost and in the start
  budgets ([ADR-0014](0014-the-quality-bar-measured-against-the-market.md)).
- Meters still reach the screen within one frame of the sound heard: the shared memory adds no
  wait.
- The plugins' compositor is a third kind of process beside the engine and the plugin hosts, at
  the stage of plugin GUIs inside the window.
