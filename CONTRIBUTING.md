# Contributing to flashdb

This repository is a research record as much as a codebase: the claim it exists to test has been measured,
and one part of it was lost. Contributions are welcome in that spirit — a change that makes a claim more
decidable is worth more than one that adds a feature.

## Scope

- The engine is `engine/` (Rust), the workload harness is `harness/` (Python 3.12), the measurement runs are
  `scripts/`, the device calibration is `bench/`, and the doc base is `docs/`. Read `docs/0_ROADMAP.md`
  before proposing anything large: it holds the milestones, the decisions in force and the non-goals.
- Out of scope by decision, not by neglect: general L2 co-location, multi-node operation, a query surface
  wider than the committed `cust → take` block graph, and any performance claim taken under foreign load.
- Facts live in the file whose question they answer (`docs/1_PROBLEM.md` … `docs/9_IMPLEMENTATION.md`);
  `docs/0_STAGING.md` is the only place an unfiled fact may sit. `scripts/check-docs.py` enforces the
  structural half of that rule.

## What must pass

```sh
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      test --workspace --offline
cd harness && env -u VIRTUAL_ENV uv run --with pytest pytest -q
python3 scripts/check-docs.py
```

A behaviour change needs the test that fails without it. A measurement change needs the record it was read
from — the run's log, its `loadavg` at start, and the revision — because a number without a record is not a
result here. Do not weaken an existing test to make a change pass; if a test encodes a decision, change the
decision in the doc that owns it first.

## Pull requests

- One concern per commit, Conventional Commits subject (`engine: …`, `harness: …`, `docs: …`, `scripts: …`).
- Say what was measured and what was not. "Unmeasured, instrument named" is a complete state; an estimate
  presented as a reading is not.
- Keep the code you touch on the documented layout. Migrating the rest is debt, and naming it as debt is
  enough.

## Issues

Report a defect with the revision, the command, and the output — including the loadavg if it is a timing
problem. For a question about the thesis, quote the record path you are reading; opening an issue at
<https://github.com/ZisIsNotZis/flashdb/issues> is the way in.
