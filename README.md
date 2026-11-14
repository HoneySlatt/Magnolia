<p align="center">
  <img src="logo/magnolia.svg" alt="Magnolia's logo: seven audio bars from coral to violet, the three tallest ending in magnolia petals" width="256">
</p>

# Magnolia

A digital audio workstation for Linux, then Windows: the composing speed of FL Studio and the
recording, editing and mixing of Pro Tools, in one program. It hosts VST3 and CLAP plugins, and
Windows plugins on Linux through a Wine bridge of its own, each out of the engine's process so that
a plugin's crash never takes the session down. It serves film and game music too: video, timecode,
loops and stems.

## State

**Designed; the engine being built.** The direction, the quality bar and the first decisions were
taken on 2026-11-06, the quality bar measured against the market on 2026-11-10. The Rust workspace
and its checks exist, the engine's time model, and a graph of nodes that plays through the JACK API
from an example program: tested on JACK2, built for PipeWire too, which the owner checks by ear. A
command line plays a test session of audio and MIDI clips from disk through JACK, or renders it
offline; a test shows, on JACK2, that the session played through the JACK backend equals its
offline render to the bit. It monitors a session's input tracks live and records the armed ones
into WAV takes, placed by the round trip's latency, which a crash leaves whole for `magnolia
recover` to repair. The first stage builds the audio engine and plugin
hosting, without an interface; the interface comes at Stage 3, on
[uwui](https://github.com/HoneySlatt/uwui), once a stable release of uwui holds what Magnolia needs.
Magnolia is a later client of uwui, not one of the programs of its first stage.

## Read next

- [`docs/ROADMAP.md`](docs/ROADMAP.md): the vision, the quality bar, the stages.
- [`docs/PLAN.md`](docs/PLAN.md): what is being built now.
- [`docs/adr/`](docs/adr/README.md): the decisions and why.
- [`docs/uwui-needs.md`](docs/uwui-needs.md): what Magnolia asks of its UI framework.
- [`docs/survey.md`](docs/survey.md): the field, with sources.
- [`AGENTS.md`](AGENTS.md): the rules for agents working here.

## Build

Every check runs in the development shell:

```sh
nix develop --command scripts/check.sh
```

[`CONTRIBUTING.md`](CONTRIBUTING.md) says how to build, test and run each check alone.

## Play a sine

The JACK backend's example plays a quiet sine (-20 dBFS) on PipeWire, at a period of 64 frames,
connected to the sound card, crossfading to another sine every two seconds so that a click would
be heard; then it says whether its audio thread ran in real time, and prints the engine's figures:

```sh
nix develop --command pw-jack -p 64 cargo run --release -p magnolia-jack --example play -- --connect
```

`pw-jack` comes with PipeWire's JACK support, outside the development shell. Without `--connect`,
it plays to no port; connect it with a patchbay such as `qpwgraph`. Its other options are in
[`CONTRIBUTING.md`](CONTRIBUTING.md#playing-through-jack).

## Play a test session

The command line, `magnolia`, plays a test session of audio and MIDI clips from disk through JACK,
then prints the engine's figures, or renders it into a WAV file. Generate the small session, about
8 seconds, into a new folder; play it on PipeWire, connected to the sound card; render it:

```sh
nix develop --command cargo run --release -p magnolia-session --example generate -- small /tmp/magnolia-small
nix develop --command pw-jack -p 64 cargo run --release -p magnolia-cli -- play /tmp/magnolia-small/session.json --connect
nix develop --command cargo run --release -p magnolia-cli -- render /tmp/magnolia-small/session.json /tmp/magnolia-small.wav
```

`magnolia record <session.json> <folder>` plays a session and records its armed input tracks into
the folder; `magnolia recover <folder>` repairs the takes a crash left there. `magnolia --help`
gives the options; [`CONTRIBUTING.md`](CONTRIBUTING.md#playing-and-rendering-a-session) says more.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE). The logo is not under it: its author keeps its rights,
and [`logo/README.md`](logo/README.md) says how it may be shown.
