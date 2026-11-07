# 0011. Magnolia no longer a program of uwui's first stage

- Status: Accepted; amends [0006](0006-the-interface-on-uwui-after-the-engine-and-the-project.md)
  (Magnolia among uwui's first programs; what its interface's stage waits for) and
  [0010](0010-the-quality-bar.md) (the screen of reference, Magnolia's own)
- Date: 2026-11-07

## Context

[ADR-0006](0006-the-interface-on-uwui-after-the-engine-and-the-project.md) made Magnolia one of the
programs of uwui's first stage, with Opium, its Android prototype and Pluto's client, as SlimeEngine's
[ADR-0203](https://github.com/HoneySlatt/SlimeEngine/blob/main/docs/adr/0203-pilots-replaced-pluto-and-a-daw.md)
decided. Magnolia's Stage 3, its interface, was to be built as uwui's first stage allowed it.

The owner decided on 2026-11-07 that Slatty, the launcher, takes Magnolia's place among uwui's first
programs, ported with Opium and Pluto's client. SlimeEngine's session of M6d.1b3 writes the ADR that
amends ADR-0203 accordingly; its number is not known yet.

How state changes uwui's interface has meanwhile been chosen, Magnolia's list among the inputs
(SlimeEngine's
[ADR-0205](https://github.com/HoneySlatt/SlimeEngine/blob/main/docs/adr/0205-signals-change-uwuis-interface.md)).

## Decision

**Magnolia is no longer a program of uwui's first stage.** uwui's first stage neither builds for
Magnolia nor is measured on it.

**Magnolia stays a project of the owner's, and a later client of uwui.** Its interface is still
written on uwui (ADR-0006). The stages without an interface, the engine, plugin hosting and the
project, go on as ADR-0006 orders them.

**[`uwui-needs.md`](../uwui-needs.md) stays whole.** It remains an input of uwui's choices, since
those are made for the long term; no need is removed from it.

**Stage 3 waits for a release of uwui that holds the set of features its list names first**
([What Stage 3 needs first](../uwui-needs.md#what-stage-3-needs-first)), its API declared stable.
uwui's plan does not promise that set, and no date is set. Needs of the set that uwui's own stages
have not brought by then are put to the owner, as needs of uwui, when Stage 3 is planned.

**The screen of reference is Magnolia's own.** The arrangement playing measures Magnolia's interface
at Stage 3 ([ADR-0010](0010-the-quality-bar.md)); uwui's first stage no longer measures it. uwui may
take it as a bench of its own; that is uwui's decision.

## Alternatives considered

- **Magnolia kept among uwui's first programs**: its needs built for sooner, but four programs to
  port for uwui's first stage; the owner chose Slatty instead.
- **Stages 1 and 2 paused until uwui is ready**: nothing in them needs uwui.
- **A temporary interface on another toolkit until uwui holds the set**: something to see sooner,
  but rejected by ADR-0006 for the same reasons: work thrown away.

## Consequences

- Magnolia's Stage 3 may start later than planned; Stages 1 and 2 are unchanged.
- Magnolia's needs weigh on uwui only as general needs, beside those of uwui's first programs.
- When Stage 3 is planned, the set in `uwui-needs.md` is checked against the release of uwui then
  available.
