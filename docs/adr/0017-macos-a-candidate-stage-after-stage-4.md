# 0017. macOS, a candidate stage after Stage 4

- Status: Accepted; amends [0001](0001-what-magnolia-is.md) (macOS a candidate stage, no longer
  only within reach)
- Date: 2026-11-10

## Context

[ADR-0001](0001-what-magnolia-is.md) keeps macOS "within reach, without a port": no code for it,
no measure, only the rule that platform code stays behind the engine's own interfaces
([`AGENTS.md`](../../AGENTS.md#architecture-rules)).

The owner wants Magnolia to be the best DAW on the market, and many professionals work on Macs.
uwui aims at macOS for its 1.0, and an application on uwui is to need no line of its own for a
platform. Magnolia's interface would therefore cost nothing on macOS. The cost of macOS is in the
engine:

- a CoreAudio backend;
- Audio Units, which Mac users expect beside VST3 and CLAP;
- plugin windows from another process: macOS has no public way to embed them, so they stay
  floating;
- signing and notarisation, with a paid Apple developer account;
- a machine with Apple Silicon to measure on: the owner's Mac is a virtual machine of an Intel Mac.

## Decision

**macOS joins the stages whose order is decided at the end of Stage 4**, beside film and games,
the sampler, plugin GUIs inside the window, Windows, stem separation and ARA.

**Its conditions are written now:**

- an Apple Silicon machine to measure on;
- Audio Units hosted;
- signing and notarisation;
- plugin windows floating.

**Nothing changes before.** No macOS code is written, and nothing is measured on macOS. The rule
that platform code stays behind the engine's interfaces keeps the port possible.

## Alternatives considered

- **Within reach only**: no promise. But "best on the market" without macOS stays narrow, and the
  question would come back.
- **macOS promised before 1.0**: a clear ambition against Logic and Ableton Live. But it commits
  the costs above, hardware to buy among them, before anything is measured.

## Consequences

- The ROADMAP's list of later stages names macOS, with its conditions.
- Choosing macOS at Stage 4's end means buying an Apple Silicon machine and an Apple developer
  account; the owner decides then.
- The modifier of shortcuts, Ctrl or Cmd, follows uwui's primary modifier, so shortcut files need
  no change for macOS ([uwui-needs.md](../uwui-needs.md#platforms-license-and-the-light-sensor)).
