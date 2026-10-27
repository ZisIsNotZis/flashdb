# 90 — the M5 postgres leg's server: present, not the measured version, and now startable and checked

Root's answer of 2026-10-23 says the box carries postgres and this tree had written it as absent. This
ticket is the environment read with its moment and method, the one command that starts and checks the
M5 server, the setup/connection/migration proof taken through it, and the exact place the quiet window
still stops the work. No throughput number is measured or claimed here.

## WP-15 (2026-10-27) — the server is the M1 user-space 18.6, not the vanished container

**Current fact, read 2026-10-27T21:37 +08 on `z`, user `z`:** `docker version` is 29.9.1 but
`docker ps -a` and `docker images` are both empty; `pull-postgres18.sh` fails with an i/o timeout to
Docker Hub (rc=1, `logs/pull-postgres18-20261027.out`, mirror list `[]`). The M1 record's user-space
extraction survives at `/var/tmp/m1-pg/`: the `postgresql-18 18.6-3build2`, `postgresql-client-18`,
`postgresql-common` and `libpq5` debs under `debs/`, the extracted tree under `root/`, the cluster
under `data/`. It is started by `scripts/baseline-env.sh`, which now defaults to that root
(`M1_PG_ROOT=/var/tmp/m1-pg`, `M1_PG_PORT=55432`) and has a `check` action that reads the version and
the M1 record's settings block back from the running server. The container script
`.scratch/90-m5-postgres-env/postgres-server.sh` is superseded (its header says so), as is
`smoke-migration.sh` (it drove that container; the current setup/serve/oracle proof is `run.sh`);
the 16.15 section below is the historical record of that route.

What changed in the chain:

- `scripts/baseline-env.sh`: the header states the present route and that Docker is not a dependency;
  the default root is the M1 extraction (`/var/tmp/m1-pg`), port 55432; `SETTINGS` is the one home of
  the M1 block, rendered by `write_config` and compared by `check`; `check` refuses any setting that
  differs and a `M1_PG_REQUIRE` version that does not match, naming each difference. `download`
  follows a symlinked `debs/` (`find -L`), which is what lets an override root reuse the extraction
  offline instead of reaching for apt.
- `scripts/m1-baseline-run.sh`: the postgres branch no longer calls Docker. It stops any earlier
  server through `baseline-env.sh stop`, starts the cluster inside its own `systemd-run` scope (so the
  128 MiB cap covers the server and the client stays outside), calls `baseline-env.sh check`, writes
  `server_version`/`version`/`data_directory` into `postgres-server.jsonl` from that read-back, and
  stops through the same `baseline-env.sh stop` in a trap. `restore_arm` stops the server and prints
  `stopped: data=...` before the snapshot is written. `M1_PG_ROOT` defaults to `/var/tmp/m1-pg`,
  `M1_PG_PORT` to 55432, and `M1_PG_REQUIRE` to **18.6** (override it to claim another version).
- `scripts/m1-ladder-run.sh` and `scripts/m1-controlled-run.sh` already build their clusters through
  `baseline-env.sh`; they call the fixed `m1-baseline-run.sh`, so no container path remains.

Evidence (loadavg is line 1 of every log; `run.sh` produces every row except the override root,
whose command is in the row):

| what | command | result | log |
|---|---|---|---|
| matching check | `baseline-env.sh check` | version `18.6 (Ubuntu 18.6-3build2)`, every setting matches, rc=0 | `logs/check-18.6.out` |
| version mismatch | `M1_PG_REQUIRE=16.15 baseline-env.sh check` | `version mismatch: the server reports "18.6 (Ubuntu 18.6-3build2)", M1_PG_REQUIRE="16.15"`, rc=1 | `logs/check-version-mismatch.out` |
| setting mismatch | `PGOPTIONS='-c work_mem=8MB' baseline-env.sh check` | `mismatch: work_mem reported "8MB", the M1 record says "4MB"`, rc=1 | `logs/check-setting-mismatch.out` |
| one-command arm | `m1-baseline-run.sh --mode full --arms postgres --requests 200 --rate 50 --skip-build --workdir runs/full-18.6` | check passes, oracle PASS, rc=0 | `logs/arm-18.6-full.out` |
| snapshot | `--mode instance ... --snapshot-dir .../snapshot --instance-root .../live` | 65,970,999 B, 1,298 files, sha256 in `snapshot/postgres.json` | `logs/chain-18.6-instance.out` |
| restored arm | `--mode arm ...` | `stopped: data=/var/tmp/m1-pg/data`, `restore postgres: ... (sha256 verified)`, check, oracle PASS, rc=0 | `logs/chain-18.6-arm-restored.out` |
| runner refusals | same arm with `M1_PG_REQUIRE=16.15` / `PGOPTIONS='-c work_mem=8MB'` | named difference, rc=1, no arm run | `logs/chain-18.6-require-mismatch.out`, `logs/chain-18.6-setting-mismatch.out` |
| override root | a root whose `root/` and `debs/` are symlinks to `/var/tmp/m1-pg`, port 55433: `setup`, `start`, `check`, `stop` | offline setup from the symlinked debs, check passes, rc=0 | `logs/override-root.out` |

The arm record's `postgres_settings.server_version` is `18.6 (Ubuntu 18.6-3build2)` and
`postgres-server.jsonl` carries the same value with `settings_checked: true`; the restored record's
`restore.verified` is `true` and its `sha256` equals the snapshot digest. Full records are under
`runs/full-18.6/` and `runs/chain-18.6/` (the 66 MB snapshot and trace are regenerable and
`runs/.gitignore` keeps them out of git).

**This is a chain check, not a rate.** The logs start at loadavg 9.6-11.0 (1-minute), far above the
quiet-window trigger of under 2, so the 200-request / 50 rps row is a window rule and no throughput
number is claimed; the three-arm comparison still waits for the granted window.

Still unverified: the three-arm comparison and any rate (quiet window unmet), and the same chain
against a different extracted major version (none is installed; `M1_PG_REQUIRE` is what would refuse
it). The 2026-10-23 sections below remain the record of the container route and the 16.15 reading.

## What is true at the moment it was read

All reads 2026-10-23 between 20:10 and 20:24 +08:00, on `z3`, user `z`. Log:
`logs/env-read-2026-10-23T2012.out` (the first block) and `logs/up-16.15.out` (the M5 server).

| fact | value | read with |
|---|---|---|
| container carrying postgres | `jolly_lamarr`, image `postgres:16`, up 4 days, `0.0.0.0:5432` | `docker ps --format '{{.Names}} {{.Image}} {{.Ports}} {{.Status}}'`; `ss -ltn \| grep 5432` |
| its version | `PostgreSQL 16.15 (Debian 16.15-1.pgdg13+2) on x86_64-pc-linux-gnu` | `docker exec jolly_lamarr psql -U postgres -tAc 'select version()'` |
| its settings | `shared_buffers=128MB`, `max_connections=100`, `work_mem=4MB`, `maintenance_work_mem=64MB`, `fsync=on`, `synchronous_commit=on`, `wal_level=replica`, `max_wal_senders=10`, trust auth, **no memory cap** (`HostConfig.Memory` 0) | `select name, setting, source from pg_settings where name in (...)`; `docker inspect` |
| 18.x on the box | **absent at 20:24**: not in `docker images`; `docker pull postgres:18` through the daemon's mirror `docker.1ms.run` ran 20:19-20:24 and moved no layer past "Pulling fs layer" (attempt 2, log `logs/pull-postgres18-attempt2.out`), so it was stopped and `up` resumes it. A direct `curl https://registry-1.docker.io/v2/` timed out at 25 s | `docker images`; `docker info` (mirrors); `curl` |
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

## The chain, closed (2026-10-23 20:31) — and the arm ran at 16.15 as a chain check

`scripts/m1-baseline-run.sh`'s postgres branch no longer starts a host cluster the box does not have.
It now starts the checked container:

- `down` before each step, then `postgres-server.sh up` — pull if the image is absent, run under
  `--memory`, wait for `pg_isready`, then check **every** setting against the M1 record and refuse a
  mismatch (and refuse a version `M5_PG_REQUIRE` does not name). `env` hands the harness the endpoint
  and the version read back from the server.
- The container's `HostConfig.Memory` is compared with `--cap-mib` and the step refuses another cap;
  the lifetime peak is read from the container's cgroup before the container is removed, so the run
  record's `arm_scope_peak_bytes` is the container's, not a scope's.
- `restore_arm` removes the container **before** the harness writes the snapshot into
  `$M1_PG_ROOT/data`, so the restore's digest check covers the bytes the next start serves.
- The data directory is bind-mounted at the host path itself (`-v $ROOT/data:$ROOT/data`,
  `PGDATA=$ROOT/data`): `show data_directory` then reports the path the harness restored, which is
  what its restored-run check compares. Verified 2026-10-23T20:30, log `logs/up-16.15.out`.
- `M1_PG_ROOT` defaults to `/var/tmp/m5-pg`; no host `pg_ctl`, no debs, no system package.

One command ran the arm through the chain at 16.15:

```
M5_PG_IMAGE=postgres:16 M5_PG_REQUIRE=16.15 bash scripts/m1-baseline-run.sh \
  --mode full --arms postgres --requests 200 --rate 50 --skip-build --workdir .tmp/m5-arm-16.15
```

Log `logs/arm-16.15-chain-203101.out`. Read back: `postgres_settings.server_version` =
`16.15 (Debian 16.15-1.pgdg13+2)` in `run-postgres.json`; `postgres-server.jsonl` carries
`image=postgres:16`, `require=16.15`, `server_version=16.15`, `data_directory=/var/tmp/m5-pg/data`;
the settings block checked; oracle PASS; cap 128 MiB; serving peak 36.8 MiB. **Loadavg at the arm's
start was 3.77 (trigger: under 2 for a minute), so this is a chain check and not a throughput
measurement** — its 200-request 50 rps row is a window rule, not a rate, and throughput stays
`unmeasured`.

**Correction, same class as root's message: a fact read at 20:24 was already false at 20:30.** The
`psycopg[binary]==3.3.6` pin does resolve offline on this box. Read 2026-10-23T20:30:
`uv run --offline --with 'psycopg[binary]==3.3.6' python -c 'import psycopg; print(psycopg.__version__)'`
→ `3.3.6` (`Installed 3 packages` from the cache, no network); `psycopg-binary` is in the uv cache.
The pin was left as it is; the earlier "does not resolve offline" line is wrong and this replaces it.

### The restore path, exercised

`--mode instance` built the pre-load instance and snapshotted it (`65,201,798 B`, 1,294 files,
sha256 `b0c8efef…de2177`, log `logs/chain-instance-16.15.out`), then `--mode arm` restored it
(digest verified, 0.5 s), started the container **on the restored directory**, passed the harness's
`data_directory` check (`/var/tmp/m5-pg/data` on both sides) and returned oracle PASS (log
`logs/chain-arm-restored-16.15.out`). Getting there found one more path bug: the chain resolved
`$WORKDIR` to an absolute path but not `--snapshot-dir`/`--instance-root`, and the harness `cd`s into
`harness/` — so a relative path silently landed under `harness/.tmp/`. Both are resolved once now,
and the payloads get the absolute `M1_INSTANCE_ROOT`.

## The quiet window opened for ~2.5 minutes, and one block is measured

Read 2026-10-23: the 1-minute loadavg was 1.84 at 20:32:35 and 0.95 at 20:34:57, with no other agent
writing in this tree — the trigger (under 2 for one minute) held. Inside it, one block at 200 rps:

```
M5_PG_IMAGE=postgres:16 M5_PG_REQUIRE=16.15 bash scripts/m1-baseline-run.sh \
  --mode full --arms postgres --requests 4000 --rate 200 --skip-build \
  --workdir .tmp/m5-arm-16.15-200rps
```

Log `logs/arm-16.15-200rps-quiet-203431.out`; `loadavg.jsonl` in that work directory reads
`0.92 2.54 2.94` at the arm's start and `0.95 2.39 2.88` at its end, so a reader sees that the
1-minute reading was the quiet one and the smoothed ones were not.

| quantity | value |
|---|---|
| offered / achieved | 200 / 199.87 rps (99.9%) |
| sustained | yes — 21 windows, 0 rejected, lag ceiling 10,000 µs p99 |
| lag p99 / service p99 | 4 µs / 41,652 µs |
| serving peak / arm scope | 39.5 MiB / 41.4 MiB under a 128 MiB cap |
| oracle | PASS |
| version | 16.15 (Debian 16.15-1.pgdg13+2), read back from the server |

What it is not: the three-arm comparison (sqlite and flashdb-declared were not run in this window),
and not comparable to ticket 82's rung-200, which ran 200,000 customers / 2,650,000 SKUs on postgres
18.6. The version is part of the number.

## The 18.x pull: one command, repeatable, and where it stands

```
bash .scratch/90-m5-postgres-env/pull-postgres18.sh          # one attempt
nohup bash .scratch/90-m5-postgres-env/pull-postgres18.sh & # same command, in the background
```

Each attempt resumes: the daemon keeps the layers it already holds. Started 2026-10-23T20:29, log
`logs/pull-postgres18-20261023.out` (append-only, one header per attempt). Read 20:32: `postgres:18`
still absent from `docker images`; the attempt sits at "Pulling fs layer" and has completed no layer.
If it lands before the end of the day: `M5_PG_IMAGE=postgres:18 M5_PG_REQUIRE=<the version
`select version()` reports>` `postgres-server.sh up`, then the same arm command with
`--workdir .tmp/m5-arm-18.x`, and the result names the 18.x it read — never "18.6" inherited from
ticket 82. If it does not land, the 16.15 arm above stands as its own measurement, with its version
stated wherever it is quoted.

## What this does not settle

- **The version a comparison may report.** The 16.15 arm is in hand and its version is read back;
  the 18.x arm waits on the pull. A later reader may not carry the 18.6 conclusion onto a 16.15
  result: the M5 comparison's meaning moves with the version, so the version travels with the number.
- **Throughput on any version.** Not measured, and the quiet window is still not held: `uptime` at
  the arm's start read 3.77 / 4.03 / 3.40 and at 20:31 3.57 / 3.98 / 3.39. `.scratch/89-m3b-paired-200/`'s
  gate and the M5 comparison wait for the trigger unchanged. The trigger is not lowered to get a
  number.

## Stale environment facts this read corrects, with their moment

- `scripts/baseline-env.sh:4` and `.scratch/80-external-baselines/env.md`: "Docker Hub is unreachable
  and no images are local". Read 2026-10-23T20:14: `postgres:16` is local; the daemon has a registry
  mirror (`docker.1ms.run`) that serves pulls slowly; direct Docker Hub is still unreachable (curl
  timeout 25 s). The header now carries both reads instead of the 2026-10-17 one as a present fact.
- `docs/9_IMPLEMENTATION.md`, `0_ROADMAP.md` §Known blockers, `9_IMPLEMENTATION.md` row `PLAN-postgres-absent`:
  "postgres is not installed" → rewritten to the read above.
- `README.md`: the external loss now names **postgres 18.6 (Ubuntu 18.6-3build2)** with the record
  that carried it, and the roadmap's footprint line says which version it is against.
