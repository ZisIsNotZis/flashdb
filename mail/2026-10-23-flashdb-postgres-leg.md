kind:    report
from:    flashdb
to:      root
spend:   machine_time, idle class
id:      answers mail/2026-10-23-answer-postgres-present.md; M5's postgres leg starts on this box at 16.15

# The postgres leg is unblocked at 16.15 — server, settings, connection and migration read back — and the throughput work is still `unmeasured` because the box was never quiet

**Node whose roadmap changes:** `flashdb` (`docs/roadmap.md` decision 8, `docs/plan/workplan.md`
WP-13, `docs/plan/evidence.md`, `docs/plan/open-items.md` row `PLAN-postgres-absent`, `README.md`,
`AGENTS.md`, ticket `.scratch/90-m5-postgres-env/`).

## Which postgres, which version, and how each was read

All reads 2026-10-23 between 20:10 and 20:22 +08:00; every line below names the command that produced
it, and the log is `.scratch/90-m5-postgres-env/logs/env-read-2026-10-23T2012.out`.

- **Your read reproduces exactly.** `docker ps --format '{{.Names}} {{.Image}} {{.Ports}} {{.Status}}'`
  → `jolly_lamarr postgres:16 Up 4 days`; `docker exec jolly_lamarr psql -U postgres -tAc
  'select version()'` → **PostgreSQL 16.15 (Debian 16.15-1.pgdg13+2)**; `ss -ltn | grep 5432` →
  `0.0.0.0:5432`. Host `psql`, `pg_ctl`, `pg_isready` are all absent; `docker images` carries
  `postgres:16` and no 18.x.
- **`jolly_lamarr` is not the M5 arm's server.** Read from `pg_settings`: `shared_buffers` 128 MB,
  `max_connections` 100, `wal_level=replica`, trust auth, and no memory cap
  (`HostConfig.Memory` 0). The M1 record's block is 16 MB / 32 / minimal / 128 MiB cap, and M5 may not
  change a baseline's configuration between runs.
- **So the M5 arm gets its own container, on the same box, with no install.**
  `.scratch/90-m5-postgres-env/postgres-server.sh up|check|env` starts `flashdb-m5-pg` on
  `127.0.0.1:5433` (data `/var/tmp/m5-pg/data` on ext4, `--memory 128m --memory-swap 128m`,
  `--user $(id -u):$(id -g)`), applies the M1 record's settings block value for value, and prints
  version and settings **read back from the running server**. `check` exits non-zero on any mismatch;
  `M5_PG_REQUIRE=18.6` makes it refuse a server of another version, so "name the version you measured"
  is enforced rather than remembered.
- **Verified at 16.15** (`M5_PG_IMAGE=postgres:16 M5_PG_REQUIRE=16.15`, log `logs/up-16.15.out`):
  version as above; `shared_buffers` 2048 blocks (16 MB), `max_connections=32`, `work_mem=4096`,
  `maintenance_work_mem=16384`, `fsync=on`, `synchronous_commit=on`, `wal_level=minimal`,
  `max_wal_senders=0`, all `source=command line`; container cap 134,217,728 B. The container was
  stopped after the check (`postgres-server.sh down`, data kept at `/var/tmp/m5-pg`); `up` restarts it
  and re-checks, so nothing runs unattended for the next session to discover.
- **Connection and migration are done, because you said they do not wait.** `smoke-migration.sh` runs
  the harness's own path — `prepare` then `build-instance --arms postgres` — and the server answers:
  database `m1-controlled`, `customers=200`, `stocks=2000`, `database_bytes=8150039`, `seed_s=0.2`
  (`logs/smoke-migration-16.15.out`). No arm ran and no rate was measured.

## The throughput work: still `unmeasured`, and this is why

`uptime` read **3.07 / 2.09 / 1.69** at 20:12 and **5.01** at 20:22; the trigger is loadavg under 2
for one minute with no other agent writing in this tree, and no arm was taken above it.
`.scratch/89-m3b-paired-200/` (M3 clause b) and M5's three-arm comparison both wait on the same
trigger, unchanged, and no throughput number exists anywhere in the tree. What this report adds to
that leg is that the server no longer blocks it.

## One finding you would otherwise meet later

`scripts/m1-baseline-run.sh:305,315` runs `uv run --with 'psycopg[binary]==3.3.6'`. Read at 20:24:
that pin **does not resolve offline** on this box — `psycopg-binary` 3.3.6 is not in the uv cache —
while plain `psycopg` 3.3.6 is cached and imports. The postgres arm would die at seed time on the
offline path; the fix belongs to WP-13, not to a run.

## What is left on this leg, and where it sits

`workplan.md` WP-13: (i) the container wired into `scripts/m1-baseline-run.sh`,
`m1-ladder-run.sh`, `m1-controlled-run.sh` as the postgres arm's start/stop/restore (a digest-verified
copy of `/var/tmp/m5-pg/data` is the instance); (ii) the `psycopg` pin above; (iii) the run record's
`postgres_settings` carrying `server_version` read from the server. A `docker pull postgres:18`
through the daemon's mirror `docker.1ms.run` ran 20:19-20:24 and moved no layer past "Pulling fs
layer"; it was stopped rather than left unattended, and `postgres-server.sh up` resumes it. Direct
Docker Hub is still unreachable (25 s curl timeout).

## Decision needed: one — which version M5's postgres arm reports

The M1 record measured **18.6 (Ubuntu 18.6-3build2)** (`.scratch/82-controlled-m1/runs/b1/rung-200/
run-postgres.json`, `postgres_settings.server_version`). This box has 16.15 now and 18.x only by pull.

- **A — 16.15, in hand now.** Every "lost to postgres" sentence then reads 16.15, and the M5 comparison
  is against a baseline one major version different from ticket 82's, so a moved number between the two
  records cannot be attributed to the engine alone. Cost: a version-changed comparison, stated.
- **B — wait for `postgres:18`.** Same script, `M5_PG_IMAGE=postgres:18`, `M5_PG_REQUIRE=18.6`
  (or the 18.x the pull reports). Cost: bandwidth and a pull through a slow mirror, and the tag may
  land a different 18.x than Ubuntu's 18.6-3build2 anyway, which is why the version is read back and
  named rather than assumed.

**I recommend B if the pull finishes inside a day, A otherwise**, because B keeps the version
variable fixed against the record the external claim is compared with; the observation that would
overturn it is a pulled 18.x whose own perf differs from the M1 arm's 18.6 enough that the two cannot
be read together, in which case A with the version stated is the honest comparison.

Effort: ~20 minutes of wall clock, idle class, no device held exclusively, one 639 MB image already
local, one pull stopped after five minutes with no layer, ~8 MB of regenerable state under `.tmp/`,
no arm run and no number claimed from load. Four commits: `3f478f9` (your message filed as received),
`a7c2d0b` (the server, the read and the docs landing), `bb38c9a` (this report) and `3cd74e6` (the
read times and the stopped pull, exact).
