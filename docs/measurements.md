# Measurements

The figures measured against the [quality bar](ROADMAP.md#quality-bar), newest milestone last. Each
figure names its machine, its tools' versions, its date and how it was taken, so that anyone can
take it again.

## M1.1 — The workspace and its checks

Taken on 2026-11-10 on the high tier (AMD Ryzen 7 5800X, 8 cores), with Nix 2.34.9, rustc 1.99.1,
the nightly 1.101.0 of 2026-11-05 and cargo-deny 0.20.2. Times are wall time from `time`, each run
three times; the figure is the median.

| What | Time | How |
|---|---|---|
| Entering the development shell, already built | 0.15 s | `nix develop --command true` |
| A full check, `target/` removed first | 1.8 s | `rm -rf target; nix develop --command scripts/check.sh`, one run |
| A full check, nothing changed | 1.4 s | `nix develop --command scripts/check.sh` |
| The CI job | not yet measured | the job's time on GitHub, after the first push of the workspace |

The workspace holds one crate without dependencies, so these figures say little yet. The budget is
in the [quality bar](ROADMAP.md#budgets): a full check from a clean build within 3 minutes on the
high tier, the same figure as a build from scratch.
