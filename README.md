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

**Designed, not built.** The direction, the quality bar and the first decisions were taken on
2026-11-06. No code exists yet. The first stage builds the audio engine and plugin hosting, without
an interface; the interface comes at Stage 3, on [uwui](https://github.com/HoneySlatt/uwui), once a
stable release of uwui holds what Magnolia needs. Magnolia is a later client of uwui, not one of the
programs of its first stage.

## Read next

- [`docs/ROADMAP.md`](docs/ROADMAP.md): the vision, the quality bar, the stages.
- [`docs/PLAN.md`](docs/PLAN.md): what is being built now.
- [`docs/adr/`](docs/adr/README.md): the decisions and why.
- [`docs/uwui-needs.md`](docs/uwui-needs.md): what Magnolia asks of its UI framework.
- [`docs/survey.md`](docs/survey.md): the field, with sources.
- [`AGENTS.md`](AGENTS.md): the rules for agents working here.

## Build

There is nothing to build yet. The checks of the documentation run in the development shell:

```sh
nix develop --command scripts/check.sh
```

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE). The logo is not under it: its author keeps its rights,
and [`logo/README.md`](logo/README.md) says how it may be shown.
