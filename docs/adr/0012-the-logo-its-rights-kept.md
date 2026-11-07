# 0012. The logo, its rights kept

- Status: Accepted; amends [0002](0002-gpl-3-0-or-later-copyright-kept.md) (the logo not under the
  GPL)
- Date: 2026-11-07

## Context

The owner made Magnolia's logo and added it to the repository on 2026-11-07, in `logo/`.
[ADR-0002](0002-gpl-3-0-or-later-copyright-kept.md) puts the whole of Magnolia under
GPL-3.0-or-later, and plans to protect the name and the logo, on which the sale of binaries rests.
A logo published under the GPL could be taken by anyone, a paid fork included, and that cannot be
undone once published. uwui's logo is kept out of its code's licenses for the same reason
(SlimeEngine's [ADR-0183](https://github.com/HoneySlatt/SlimeEngine/blob/main/docs/adr/0183-uwuis-logo.md)).

## Decision

**The logo is not under the GPL.** Its author keeps its rights. It may be shown unchanged to refer
to Magnolia; it may not be the logo of another project, a fork included, nor suggest that a project
is Magnolia or endorsed by it. [`logo/README.md`](../../logo/README.md) says so beside the file.

**The trademark policy that ADR-0002 plans** covers the logo with the name, before the first binary
is sold.

## Alternatives considered

- **The logo under the GPL with the code**: one license for the whole repository, but any fork,
  paid ones included, could take the logo, against what the binaries' sale rests on.

## Consequences

- The repository holds one file that the GPL does not cover; the README says so.
- A fork must change the logo, as it must change the name under the trademark policy.
