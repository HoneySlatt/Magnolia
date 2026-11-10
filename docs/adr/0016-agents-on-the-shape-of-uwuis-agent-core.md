# 0016. Agents on the shape of uwui's agent core

- Status: Accepted
- Date: 2026-11-10

## Context

Magnolia has an MCP server from M1.8. Until Stage 3 it is the only way, with the command line, to
drive Magnolia ([ADR-0006](0006-the-interface-on-uwui-after-the-engine-and-the-project.md)). Every
edit is a command, and undo lives in the project's file
([ADR-0009](0009-the-project-in-sqlite-nothing-lost.md)).

uwui is to hold one core for agents
([SlimeEngine's ADR-0218](https://github.com/HoneySlatt/SlimeEngine/blob/main/docs/adr/0218-one-core-for-agents-in-uwui.md)):

- a protocol bound to no transport;
- a first message that names the agent, and the person's consent given agent by agent;
- actions declared once, each with typed parameters, a description, a permission and a family;
- rights, a preview before applying, and tools found by family.

uwui's MCP server is built on that core. SlimeEngine's milestone M6d.10 decides who owns commands,
undo and their journal. The core is not published yet.

No DAW maker ships a server for agents. Community MCP servers drive REAPER, Ableton Live and others
through their scripting interfaces, found on 2026-11-10
([survey](../survey.md#the-market-measured-2026-11-10)):

- for REAPER, `bonfire-systems/reaper-mcp` and `TwelveTake-Studios/reaper-mcp`;
- for Ableton Live, `ahujasid/ableton-mcp`.

Models know these DAWs from their training, not Magnolia.

## Decision

**M1.8's MCP server takes the shape of uwui's agent core.**

- Each action is declared once: its name, typed parameters, a description, a permission and a
  family.
- An agent is named. The user consents agent by agent and gives it its rights.
- A change is previewed before it is applied.
- The declarations are written in the core's format, as far as it is published. Magnolia switches
  to uwui's core once it is published, so that agents reach Magnolia through one interface.

**Commands and undo stay Magnolia's.** The engine runs without a window
([ADR-0015](0015-the-interface-in-a-process-of-its-own.md)). Magnolia asks uwui for a core usable
without a window, that leaves commands and undo to the application
([uwui-needs.md](../uwui-needs.md#commands-undo-agents-and-services)).

**An agent bench, from Stage 4's end.** It has three fixed tasks, given through MCP:

- a beat from a brief;
- a mix to a loudness target;
- a routing error repaired.

The same agents get the same words, against the community servers of REAPER and Ableton Live,
their versions pinned and their telemetry off. Bitwig's servers are left out: one has not changed since
2025-04-10, the other is archived ([survey](../survey.md#the-market-measured-2026-11-10)).

- Each task runs 30 times for each model. Success, tries, tokens and time are measured, the tool
  definitions counted in the tokens. The models, their versions and the date are named.
- Each task passes when Magnolia's success is not significantly below the best rival's, by a
  one-sided exact test at 5 %, and its median tokens are at most half of that rival's.
- The full bench needs a project and a loudness meter, so it starts at Stage 4's end. M1.8 has its
  own test: an agent loads, plays, records and edits a plugin's parameters through MCP alone.
- The figures are provisional until the first run.

**Documentation for agents.** Magnolia ships an `AGENTS.md` for agents that use it, and a compact
reference of its actions, generated from their declarations.

## Alternatives considered

- **A server of Magnolia's own shape, moved to uwui's later**: two protocols to write, which
  SlimeEngine's ADR-0218 rejected.
- **Waiting for uwui's core before any MCP server**: Stage 1 and Stage 2 would have no way for
  agents and tests to drive Magnolia.
- **A margin of "fewer tokens" without a figure**: it gives no measure of the lead.
- **One run per task**: two agents that each succeed 80 % of the time would pass all three tasks
  only 59 % of the time. That measures chance.
- **No rivals, only Magnolia's own tasks**: it never says whether Magnolia is the best DAW for
  agents.

## Consequences

- M1.8 reads the published state of uwui's core before writing the server.
- The bench costs tokens: 30 runs of each task, for each model, on each program.
- A community rival can improve fast. Its version is frozen until the next stage audit
  ([ADR-0014](0014-the-quality-bar-measured-against-the-market.md)).
- Switching to uwui's core waits until Magnolia can take uwui at all: uwui's crates under
  SlimeEngine's own license leave uwui at SlimeEngine's M6d.11.
