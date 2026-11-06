# 0006. The interface on uwui, after the engine and the project

- Status: Accepted
- Date: 2026-11-06

## Context

Magnolia's interface is written on uwui, the owner's UI framework in Rust, drawn by the GPU. uwui
is not ready: SlimeEngine's Stage 6d is moving it out of the engine, and it decides soon how an
application's state changes its interface. Magnolia is one of the programs of uwui's first stage,
with Opium, its Android prototype and Pluto's client (SlimeEngine's
[ADR-0203](https://github.com/HoneySlatt/SlimeEngine/blob/main/docs/adr/0203-pilots-replaced-pluto-and-a-daw.md)).

Meadowlark, a DAW in Rust, stalled mostly on its interface: no Rust toolkit then had what a DAW
needs ([the survey](../survey.md#rust-daws)). The engine, plugin hosting and the project need no
interface to be built and tested.

## Decision

**The interface is written on uwui**, in its dark, dense look, one window with docked panels. What
Magnolia asks of uwui is listed in [`uwui-needs.md`](../uwui-needs.md), general needs apart from
Magnolia's own.

**The order of the first four stages**, whose contents the [ROADMAP](../ROADMAP.md#4-stages)
holds:

1. the engine and the plugins, without an interface;
2. the project, without an interface;
3. the interface on uwui, as uwui's first stage allows it, with a simple sampler so that the step
   sequencer makes sound;
4. recording and audio editing, Pro Tools' side, with the built-in mixing essentials.

**Until Stage 3, Magnolia is driven by a command line and an MCP server**, which tests and agents
use too. No temporary interface is built, in egui or anything else: it would be thrown away.

**The order after Stage 4 is decided at its end**, among: film and games (video, timecode, game
exports, the mastering document), the sampler and the drum machine, plugin GUIs inside the window,
Windows, stem separation and ARA.

## Alternatives considered

- **The project before the plugins**: the model first, but the largest risk, Wine, met later.
- **A temporary interface until uwui is ready**: something to see sooner, but work thrown away, and
  a second interface to keep in step meanwhile.
- **Waiting for uwui before any work**: nothing that needs no interface would be built.

## Consequences

- Nothing is seen on screen for two stages, except plugin GUIs.
- The engine and the project are whole and tested when the interface arrives, which makes the
  interface thin.
- Stage 3 depends on uwui's first stage. The smallest set of uwui features that lets Magnolia be
  played is given to the owner for uwui's plan ([`uwui-needs.md`](../uwui-needs.md)).
- The command line and the MCP server stay after Stage 3, for tests, agents and scripting.
