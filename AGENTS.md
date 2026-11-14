# AGENTS.md

Rules for AI agents working in this repository. Read this file fully at the start of every session.

## Project

Magnolia is a DAW of the class of FL Studio and Pro Tools, written in Rust, for Linux first, then
Windows. It hosts VST3 and CLAP plugins, Windows plugins on Linux through a Wine bridge of its own,
each out of the engine's process. Its interface will be written on uwui, the owner's UI framework,
which is not ready yet. It is licensed under GPL-3.0-or-later, its copyright held by the owner
alone ([ADR-0002](docs/adr/0002-gpl-3-0-or-later-copyright-kept.md)).

Its real state: the design is done (ADRs 0001 to 0032); the workspace and its checks exist (M1.1);
the engine holds the time model (M1.2), plays a graph through JACK (M1.3), plays a test session
of audio clips and MIDI from disk, delays compensated, through the `magnolia` command (M1.4), and
monitors and records input tracks into takes that survive a crash (M1.5); M1.3 to M1.5 are built
and wait for the owner's measures by hand.

Agents develop it; the owner sets its direction and validates its decisions. Anyone must be able to
understand it from the repository alone. Documentation is part of every deliverable.

## Read before working

- `docs/PLAN.md`, its *Current* section and the current milestone: what to do now.
- When planning, or when the task touches a decision: `docs/ROADMAP.md` (direction, stages, the
  quality bar), and `docs/adr/`, the decisions already made. `docs/ARCHITECTURE.md`: the crates and the flows
  across them.
- `docs/uwui-needs.md` when the work touches the interface; `docs/survey.md` for the field's facts
  and their sources.

You remember nothing from earlier sessions. What is not written in these files is lost.

## Scope

- Work on the current milestone of `docs/PLAN.md`, or on what the owner asks. A request that changes
  the plan is written into the PLAN.
- Do not start later stages, nor prepare abstractions for them.
- When experience contradicts the roadmap, propose a change; once accepted, record it in an ADR.
- Never change SlimeEngine or uwui from here. What Magnolia asks of uwui goes into
  `docs/uwui-needs.md`, for the owner to carry.

## Commands

```sh
nix develop          # the development shell: every tool the checks use
scripts/check.sh     # every check of the definition of done; what CI runs
```

`CONTRIBUTING.md` holds the commands to build, test and run each check alone.

Never on your own:

- run a plugin on the owner's own Wine prefix, plugin folders or projects: use test prefixes and
  test sessions;
- activate a plugin's license or sign in to a vendor's account: the owner does it;
- download a plugin, an installer or a sample library.

## Definition of done

- `scripts/check.sh` passes.
- The program was run and behaved as expected.
- New behaviour is tested where a test is practical.
- The budgets the milestone touches are measured and met; the figures go to
  `docs/measurements.md`.
- `docs/PLAN.md` gives the new status.
- Every document the change affects is updated and reread as a newcomer would.
- Every decision a newcomer would question is an ADR.

Never report as working what was not verified. Say exactly what was not, and why. Never weaken a
check to make it pass (a skipped test, a raised budget, a silenced lint) without the owner's go.

## Architecture rules

- The engine, the project and the plugin hosts run without a window, a GPU or a network. The
  interface depends on them, never the reverse.
- No third-party plugin runs in the engine's process
  ([ADR-0003](docs/adr/0003-every-plugin-out-of-the-engines-process.md)).
- Every edit of a project is a command; undo, tests, the command line, the MCP server and
  accessibility use the same commands ([ADR-0009](docs/adr/0009-the-project-in-sqlite-nothing-lost.md)).
- Platform code (audio backends, Wine, windows of plugins) lives behind the engine's own
  interfaces, never spread through it.
- No temporary interface: until Stage 3, Magnolia is driven by its command line and MCP server
  ([ADR-0006](docs/adr/0006-the-interface-on-uwui-after-the-engine-and-the-project.md)).

## Rules the code depends on

- **The real-time path** never allocates, locks, blocks or makes a system call outside the
  allowlist ([ADR-0023](docs/adr/0023-the-real-time-allowlist.md)). RealtimeSanitizer and
  Clippy check it in CI ([ADR-0007](docs/adr/0007-the-audio-engine.md),
  [ADR-0020](docs/adr/0020-clippy-forbids-the-locks-the-sanitizer-cannot-see.md)).
- **A plugin never stalls the engine**: past its deadline, or dead, its block is silent, never
  bypassed.
- **Time is exact**: absolute positions in 64-bit time units, the engine's in 64-bit samples,
  musical positions in 64-bit ticks; never float beats; every conversion by `place` or `read`
  ([ADR-0018](docs/adr/0018-the-time-model.md)).
- **The user's data is never lost**: each gesture is a transaction; recordings survive a crash; a
  missing plugin's state is kept as it was; video is referenced, never copied.
- **Wine**: Magnolia uses the system's Wine and says plainly when its version is untested
  ([ADR-0004](docs/adr/0004-magnolias-own-wine-bridge-on-the-systems-wine.md)).
- **Licenses**: code borrowed from yabridge or Carla keeps its notice. A dependency must fit
  GPL-3.0-or-later. The VST name and logo follow Steinberg's usage guidelines, or are not used.
- **Nothing leaves the machine** without the user's consent: no telemetry.

## Code rules

- No abstraction without a second concrete use.
- No reimplementation of the standard library without a measurement.
- A new dependency needs a reason, an alternative weighed and a license checked; ask the owner.
- Every `unsafe` block has a `// SAFETY:` comment.
- Code reads plainly to someone who has never seen it: names that say what things are, one job per
  function, the obvious solution over the clever one.
- Comments say why, not what. No commented-out code.
- A bug is fixed at its cause, with the test that failed first; look for the same bug elsewhere.
- Edge cases are thought through and tested: empty, huge, malformed, concurrent, interrupted.
- Parsers of files from outside (projects, audio, MIDI, plugin state) are fuzzed.
- Search for an existing helper before writing one. One way to do a thing.
- Delete dead code and unused parameters.
- Report drift from `docs/ARCHITECTURE.md` or an ADR; do not work around it.

## Documentation

- It is written for a newcomer: plain words, short sentences, the essential first, as short as
  complete.
- It describes the code as it is now. Wrong documentation is worse than none.
- No reference to conversations the reader cannot see. Coined terms go in `docs/glossary.md`.
- Limits and debt are stated plainly.
- Each fact has one home; other documents link to it:
  - the code's documentation: contracts, limits, figures;
  - `docs/ARCHITECTURE.md`: flows across parts;
  - ADRs: why a decision was taken, what was rejected; only their status changes later;
  - `docs/PLAN.md`: status and known debt;
  - `CHANGELOG.md`, from the first release: what changed for users.
- A change that leaves a document wrong is not finished.

## Commits

- One logical change per commit, in English, its subject in the imperative.
- Never commit code that fails the checks. A check that fails on the main branch is fixed before
  any new work.

## When to stop and ask

Before a decision no ADR covers, a new dependency, a request against the roadmap or an ADR, a plan
that turns out wrong, or deleting or rewriting a large part of the code. Give the options, their
trade-offs in plain terms and a recommendation for the long term, one decision at a time.

## Reporting

A stage ends with a stage audit, and a release is preceded by a release audit; their findings go to
`docs/audits/`. Ideas outside the current work go to the *Ideas* of `docs/PLAN.md`.

At the end of each milestone, after a reviewing agent on the strongest model has checked it against
this file, the ADRs and the quality bar, report: what was done; the choices, why, and the
alternatives rejected; what to read first; the figures measured; shortcuts and debt; the next step,
on the owner's go. The next milestone starts in a new session, unless the owner says to continue.

## Working with several agents

- One milestone per agent, as `docs/PLAN.md` assigns it.
- Shared files (`docs/PLAN.md`, the build manifests): one agent at a time.
- Commit your files by their paths, never with `git add -A`.
- Delegate broad searches to subagents; keep their conclusions, not the files they read.

## Language

Everything in the repository is in English.
