# 0002. GPL-3.0-or-later, its copyright kept by the owner

- Status: Accepted
- Date: 2026-11-06

## Context

The owner wants free code and binaries for sale, as Ardour does ([ADR-0001](0001-what-magnolia-is.md)).
The license changes what Magnolia may reuse (verified on 2026-11-06, [the survey](../survey.md#licenses)):

- yabridge, the Wine plugin bridge, is GPL-3.0-or-later.
- The ASIO SDK 2.3.4 is GPL-3.0 or a proprietary license countersigned by Steinberg.
- Rubber Band, a time-stretch library, is GPL-2.0-or-later or commercial.
- The VST3 SDK is MIT since 3.8.0; CLAP is MIT; the ARA SDK is Apache-2.0. All fit under the GPL.
- uwui is MIT OR Apache-2.0, which a GPL program may use.

## Decision

**Magnolia is licensed under GPL-3.0-or-later**, the whole of it. The text is in
[`LICENSE`](../../LICENSE).

**The owner keeps all the copyright**, at least until the 1.0. No outside contribution is merged
before a contribution policy exists. Holding all the rights keeps two doors open: an exception
under the GPL's section 7, for a proprietary library the field relies on, and a permissive
license for a generic crate when it is published on its own.

**These doors open only on code the owner wrote alone.** Code borrowed from yabridge or Carla keeps
its authors' copyright and license. It lives in modules named as such, so that what can be
relicensed and what cannot stays visible.

**The name and the logo** are protected by a trademark policy, written before the first binary is
sold. The binaries' value rests on the name, the convenience and the updates, not on the code being
hard to build.

## Alternatives considered

- **GPL for the application, MIT OR Apache-2.0 now for the generic crates** (the plugin host, the
  real-time primitives): the crates would feed Rust's ecosystem early. Rejected for now: two
  licenses to track, and nothing borrowed from yabridge in those crates. Keeping the copyright
  allows it later.
- **MIT OR Apache-2.0 everywhere**, as uwui: consistent and reusable by anyone. Rejected: the
  bridge written wholly from nothing, ASIO under a contract with Steinberg, and anyone may sell a
  closed copy.
- **Closed and sold**: the owner wants free software.

## Consequences

- Ideas and code from yabridge and Carla may be reused, with their notices kept.
- ASIO may be used under the GPL on Windows, without a contract. Its SDK is GPL version 3 only, so
  a Windows build that includes ASIO is GPL-3.0 only, not "or later".
- Proprietary plugins are loaded at run time, in processes of their own, as Ardour loads them.
- Anyone may publish free builds; the trademark policy and the convenience of the official ones
  carry the sales.
- A contribution policy (a CLA, or a statement that the project stays under one copyright holder)
  is an open decision, taken before outside contributions open.
