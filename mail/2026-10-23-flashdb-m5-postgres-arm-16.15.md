kind:    report
from:    flashdb
to:      root
spend:   machine_time, idle class
id:      answers your answer on the postgres version; M5's postgres arm runs at 16.15 with the version read back, throughput stays unmeasured

# The postgres arm now runs through one command and reports **16.15, read back from its own server**; the comparison's version is stated with every number, and no throughput number exists because the box never went quiet

**Node whose roadmap changes:** `flashdb` (`docs/roadmap.md` §Current milestone and decision 8,
`docs/plan/workplan.md` WP-13 and §Known blockers, `docs/plan/evidence.md`,
`docs/plan/open-items.md` row `PLAN-postgres-absent`, `README.md` unchanged in substance,
`AGENTS.md`, ticket `.scratch/90-m5-postgres-env/`).

## The arm, at 16.15

```sh
M5_PG_IMAGE=postgres:16 M5_PG_REQUIRE=16.15 bash scripts/m1-baseline-run.sh \
  --mode full --arms postgres --requests 200 --rate 50 --skip-build --workdir .tmp/m5-arm-16.15
```

- **Version, read back from the running server:** **16.15 (Debian 16.15-1.pgdg13+2)**, from
  `select version()` inside container `flashdb-m5-pg`. It is carried in the arm record as
  `run-postgres.json:postgres_settings.server_version` and in
  `.tmp/m5-arm-16.15/postgres-server.jsonl` as `server_version=16.15`, `image=postgres:16`,
  `require=16.15`, `data_directory=/var/tmp/m5-pg/data`.
- **Settings, read back:** the M1 record's block, value for value — `shared_buffers` 16 MB,
  `max_connections=32`, `work_mem=4096` kB, `maintenance_work_mem=16384` kB, `fsync=on`,
  `synchronous_commit=on`, `wal_level=minimal`, `max_wal_senders=0`; container cap 134,217,728 B;
  oracle PASS; serving peak 36.8 MiB.
- **The log that holds it:** `.scratch/90-m5-postgres-env/logs/arm-16.15-chain-203101.out`.
- **What it is not:** loadavg at its start read **3.77 / 4.03 / 3.40**, above the trigger. It is a
  chain check — the arm executes end to end, connects, seeds, serves and verifies — and **no rate is
  claimed from it**. Its 200-request row is a window rule, not a throughput measurement. Throughput
  stays `unmeasured` on every arm and every version.

## The chain blocker and the refusal, closed

`scripts/m1-baseline-run.sh`'s postgres branch started the user-space cluster `baseline-env.sh`
builds with `pg_ctl` in a systemd scope. That cluster is gone (`/var/tmp/m1-pg` absent), the host has
no `pg_ctl`, and the box's postgres is a container, so the branch could not run at all. It now:

- starts and stops the checked container (`postgres-server.sh up|down|env`), pulling only if the
  image is absent, under `--memory`, and refuses when any of the eight settings differs from the M1
  record or when `M5_PG_REQUIRE` names a version the server does not report — the refusal is the
  script's exit code, not a printed warning;
- refuses a container whose memory cap is not `--cap-mib`, so the resident-bytes column cannot
  describe another cap;
- removes the container before a restore writes `$M1_PG_ROOT/data`, so the snapshot's digest check
  covers the bytes the next start serves;
- bind-mounts the data directory at the host path itself, so `show data_directory` equals the path
  the harness restored and the harness's restored-run check compares like with like (verified at
  20:30).

Two facts from the earlier read are now corrected, both with their moment:

1. **`psycopg[binary]==3.3.6` does resolve offline** on this box. Read 2026-10-23T20:30:
   `uv run --offline --with 'psycopg[binary]==3.3.6' python -c 'import psycopg; print(psycopg.__version__)'`
   → `3.3.6`, installed from the cache with no network. The 20:24 line saying it does not is wrong;
   the pin is unchanged and the wrong sentence is replaced in the ticket.
2. **The postgres arm's server is 16.15 and says so from the server, not from the tag.** A 16.15
   result never inherits ticket 82's 18.6 sentence.

## The 18.x pull: one command, still trying

```sh
bash .scratch/90-m5-postgres-env/pull-postgres18.sh          # one attempt
nohup bash .scratch/90-m5-postgres-env/pull-postgres18.sh & # same command, in the background
```

Every attempt resumes: the daemon keeps the layers it already holds. Started 2026-10-23T20:29; read
20:32: `postgres:18` still absent, no layer completed, log `logs/pull-postgres18-20261023.out`. If it
lands before the end of the day, the arm is re-run with `M5_PG_IMAGE=postgres:18
M5_PG_REQUIRE=<the version read back>` and the result names that 18.x — not 18.6 by inheritance. If it
does not, the 16.15 arm above stands as its own measurement and no document carries 18.6 onto it.

## The quiet-box trigger, unchanged

Read at the arm's start and again at 20:31: **3.77 / 4.03 / 3.40** and **3.57 / 3.98 / 3.39**. The
trigger stays **loadavg under 2 for one minute with no other agent writing in this tree**, and I did
not lower it to get a number. `.scratch/89-m3b-paired-200/` and the M5 comparison wait on the same
trigger; nothing in this tree can produce the window.

## Decision needed: none

Your answer settled the version question: 16.15 now, 18.6 only if the image lands and the server
reads it back. What remains blocked is the quiet box — a person's gift, not a work item.

Effort: ~25 minutes of wall clock, idle class, one background image pull (bandwidth only, no layer
completed), one 200-request postgres arm used as a chain check and not scored as a rate, ~10 MB of
regenerable state under `.tmp/`, no device held exclusively, no commit pushed.
