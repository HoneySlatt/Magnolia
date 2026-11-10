# Measurements

The figures measured against the [quality bar](ROADMAP.md#quality-bar), newest milestone last. Each
figure names its machine, its tools' versions, its date and how it was taken, so that anyone can
take it again.

## M1.1 — The workspace and its checks

Taken on 2026-11-11 on the high tier (AMD Ryzen 7 5800X, 8 cores), its load average about 5, with
Nix 2.34.9, rustc 1.99.1 and cargo-deny 0.20.2. The nightly is the one `flake.nix` pins as
2026-11-06; rustc reports it as 1.101.0 of 2026-11-05, the date of its last commit. Times are wall
time from `time`, each taken three times; the figure is the median.

| What | Time | How |
|---|---|---|
| Entering the development shell, already built | 0.15 s | `nix develop --command true` |
| A full check, `target/` removed first | 2.6 s | `rm -rf target; nix develop --command scripts/check.sh` |
| A full check, nothing changed | 1.5 s | `nix develop --command scripts/check.sh` |
| `cargo deny`, fetching the advisories | 1.0 s | `nix develop --command cargo deny --locked check` |
| `cargo deny`, without fetching | 0.6 s | the same with `--disable-fetch` |
| The CI job | 46 s | the job's time on GitHub, run 42919007937 of commit `97eaf72`, one run |

The workspace holds one crate without dependencies, so these figures say little yet. Fetching the
advisories is most of a check with nothing changed. The budget is in the
[quality bar](ROADMAP.md#floors): a full check from a clean build within 3 minutes on the high
tier, the compiling budget of a build from nothing
([ADR-0014](adr/0014-the-quality-bar-measured-against-the-market.md)).
