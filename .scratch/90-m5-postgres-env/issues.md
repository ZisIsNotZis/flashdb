# 90 — the M5 postgres leg's server: present, not the measured version, and now startable and checked

Root's answer of 2026-10-23 says the box carries postgres and this tree had written it as absent. This
ticket is the environment read with its moment and method, the one command that starts and checks the
M5 server, the setup/connection/migration proof taken through it, and the exact place the quiet window
still stops the work. No throughput number is measured or claimed here.

## What is true at the moment it was read

All reads 2026-10-23 between 20:10 and 20:28 +08:00, on `z3`, user `z`. Log:
`logs/env-read-2026-10-23T2012.out` (the first block) and `logs/up-16.15.out` (the M5 server).

| fact | value | read with |
|---|---|---|
| container carrying postgres | `jolly_lamarr`, image `postgres:16`, up 4 days, `0.0.0.0:5432` | `docker ps --format '{{.Names}} {{.Image}} {{.Ports}} {{.Status}}'`; `ss -ltn \| grep 5432` |
| its version | `PostgreSQL 16.15 (Debian 16.15-1.pgdg13+2) on x86_64-pc-linux-gnu` | `docker exec jolly_lamarr psql -U postgres -tAc 'select version()'` |
| its settings | `shared_buffers=128MB`, `max_connections=100`, `work_mem=4MB`, `maintenance_work_mem=64MB`, `fsync=on`, `synchronous_commit=on`, `wal_level=replica`, `max_wal_senders=10`, trust auth, **no memory cap** (`HostConfig.Memory` 0) | `select name, setting, source from pg_settings where name in (...)`; `docker inspect` |
| 18.x on the box | **absent at 20:28**: not in `docker images`; `docker pull postgres:18` through the daemon's mirror `docker.1ms.run` was in flight when the budget ran out (attempt 2, log `logs/pull-postgres18-attempt2.out`). A direct `curl https://registry-1.docker.io/v2/` timed out at 25 s | `docker images`; `docker info` (mirrors); `curl` |
| host client tools | `psql`, `pg_ctl`, `pg_isready` all absent | `command -v` each |

`jolly_lamarr` is therefore **not** the M5 arm's server: it is 16.15, it has no memory cap, its
`shared_buffers` is 128 MB rather than the M1 record's 16 MB, and its data directory is shared with
whatever else uses it. What it does establish is root's point: the engine is on the box, and "no
postgres here" was a stale sentence.

## The M5 server: one command, refusing what does not match the record

`postgres-server.sh up|down|check|env|status` runs a **second** container (`flashdb-m5-pg`, default
image `postgres:18`, `M5_PG_IMAGE` to change it) from a local or pulled image:

```
bash .scratch/90-m5-postgres-env/postgres-server.sh up     # pull if absent, run, wait, check
bash .scratch/90-m5-postgres-env/postgres-server.sh check  # version + every setting, read back
bash .scratch/90-m5-postgres-env/postgres-server.sh env    # export M1_PGHOST/M1_PGPORT/M1_PGUSER/...
```

- Port `5433` on `127.0.0.1`, data `/var/tmp/m5-pg/data` on the ext4 mount (not tmpfs), `--memory
  128m --memory-swap 128m`, `--user $(id -u):$(id -g)` so the tree owns its data directory without
  root.
- The settings block is the M1 record's, value for value (`scripts/baseline-env.sh:86-101`): the M5
  re-run changes the engine revision, not the baseline's configuration.
- `check` prints the read time, the method beside each value, and **exits non-zero on any mismatch**;
  `M5_PG_REQUIRE=18.6` makes it refuse a server of another version, which is how "every result names
  the version it measured" is enforced rather than remembered.

Verified at 16.15 (`M5_PG_IMAGE=postgres:16 M5_PG_REQUIRE=16.15`, log `logs/up-16.15.out`): version
read back as above; `shared_buffers` reported `2048` blocks (16 MB), `max_connections=32`,
`work_mem=4096` kB, `maintenance_work_mem=16384` kB, `fsync=on`, `synchronous_commit=on`,
`wal_level=minimal`, `max_wal_senders=0`, all `source=command line`; container cap 134,217,728 bytes.

Two launch facts found while getting there, both recorded in the script: a bind mount over
`/var/lib/postgresql` (the version-agnostic parent) is not usable without root — the entrypoint's
permission pass leaves a root-owned `data` dir the non-root server cannot chmod — so the mount is
`/var/lib/postgresql/data` with `PGDATA` pinned there; and a leftover root-owned data directory makes
`initdb` fail on `chmod`, which is why the working directory must be the account's own.

## Setup, connection, migration — taken, because they do not wait for the quiet window

`smoke-migration.sh` exercises the harness's own postgres path, not a hand-written schema: `prepare`
then `build-instance --arms postgres` through `M1_PG*`, then the counts read back from the server.

```
bash .scratch/90-m5-postgres-env/smoke-migration.sh
```

Log `logs/smoke-migration-16.15.out`: database `m1-controlled`, `customers=200`, `stocks=2000`,
`database_bytes=8150039`, `seed_s=0.2`, and the server's `select version()` printed beside them. The
harness's own count check (`expected_counts`) passed before the shell read it back. This is setup and
connection, not a measurement: no arm ran, no window opened, no rate measured.

## What this does not settle

- **The arm's version.** Ticket 82's postgres was **18.6 (Ubuntu 18.6-3build2)**. The M5 arm should
  report the same major version if it can; the `postgres:18` pull is the only route to it (no system
  package, per root) and it did not finish inside this budget. Until it does, the box's working server
  is **16.15** and any "lost to postgres" from it must read 16.15.
- **The container wired into the M1 run chain.** `scripts/m1-baseline-run.sh` and
  `scripts/m1-ladder-run.sh` start the postgres server with `pg_ctl` in a user systemd scope and stop
  it the same way; a container needs the start/stop/restore equivalents (`docker start/stop`,
  digest-checked copy of `/var/tmp/m5-pg/data`). That is work package `workplan.md` WP-13, and it is
  what makes the M5 arm one command.
- **One blocker found in the chain itself.** `scripts/m1-baseline-run.sh:305,315` runs
  `uv run --with 'psycopg[binary]==3.3.6'`, which **does not resolve offline on this box** — read at
  20:24: `psycopg-binary` 3.3.6 is not in the uv cache, while plain `psycopg` 3.3.6 is and imports.
  The postgres arm would die at seed time on the offline path; the fix belongs to WP-13.
- **Throughput on any version.** Not measured, and the quiet window is still not held: `uptime` at
  20:12 read 3.07 / 2.09 / 1.69 and no arm was taken. `.scratch/89-m3b-paired-200/`'s gate and the
  M5 comparison wait for the trigger unchanged.

## Stale environment facts this read corrects, with their moment

- `scripts/baseline-env.sh:4` and `.scratch/80-external-baselines/env.md`: "Docker Hub is unreachable
  and no images are local". Read 2026-10-23T20:14: `postgres:16` is local; the daemon has a registry
  mirror (`docker.1ms.run`) that serves pulls slowly; direct Docker Hub is still unreachable (curl
  timeout 25 s). The header now carries both reads instead of the 2026-10-17 one as a present fact.
- `docs/plan/evidence.md`, `workplan.md` §Known blockers, `open-items.md` row `PLAN-postgres-absent`:
  "postgres is not installed" → rewritten to the read above.
- `README.md`: the external loss now names **postgres 18.6 (Ubuntu 18.6-3build2)** with the record
  that carried it, and the roadmap's footprint line says which version it is against.
