kind:    report
from:    flashdb
to:      root
spend:   machine_time, idle class
id:      answers your answer on the postgres version; the M5 postgres arm runs at 16.15, one quiet-window block measures it at 200 rps, the comparison is still open

# The postgres arm runs through one command and reports **16.15, read back from its own server**; one quiet-window block measures it at 200 rps, and the three-arm comparison is still open

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
- **What it is not:** loadavg at its start read **3.77 / 4.03 / 3.40**, above the trigger. That run
  is a chain check — the arm executes end to end, connects, seeds, serves and verifies — and **no
  rate is claimed from it**. It is kept as the chain's proof, not as a measurement.

## The quiet window opened at 20:32, and the arm was measured inside it

The trigger held: the 1-minute loadavg read 1.84 at 20:32:35 and was still under 2 at 20:34:57
(0.92 → 0.95), and no other agent was writing in this tree. Inside it, one block at the comparison's
offered rate:

```sh
M5_PG_IMAGE=postgres:16 M5_PG_REQUIRE=16.15 bash scripts/m1-baseline-run.sh \
  --mode full --arms postgres --requests 4000 --rate 200 --skip-build \
  --workdir .tmp/m5-arm-16.15-200rps
```

Log `.scratch/90-m5-postgres-env/logs/arm-16.15-200rps-quiet-203431.out`; the run's own
`loadavg.jsonl` reads 0.92 / 2.54 / 2.94 at the arm's start and 0.95 / 2.39 / 2.88 at its end.

| quantity | value | condition |
|---|---|---|
| offered / achieved | 200 / **199.87 rps** (99.9%) | 4,000 requests, one 200 rps segment, 20 s |
| sustained | **yes**, 21 windows, 0 rejected | lag ceiling 10,000 µs p99 |
| lag p99 | **4 µs** | schedule lag, not service time |
| service p99 | **41,652 µs** | batch 64, one durable commit per batch |
| serving peak | **39.5 MiB** (arm scope 41.4 MiB) | 128 MiB cap, container's cgroup |
| oracle | **PASS** | inventory read back from the server |
| version | **16.15 (Debian 16.15-1.pgdg13+2)** | read back from the running server |

What this number is: the postgres arm's own reading at **16.15**, on 200 customers / 500 SKUs /
4 locations, 4,000 requests at 200 rps, one block taken inside the trigger. What it is not: the M5
comparison — `flashdb-declared` and `sqlite` were not run in this window, so no win / not-worse / loss
verdict is claimed; and it is **not comparable to ticket 82's rung-200**, which ran 200,000 customers
and 2,650,000 SKUs on postgres 18.6. A reader must not carry the 18.6 sentence or its 3-of-5 onto this
number.

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
  20:30);
- was exercised through the controlled protocol's two state steps: `--mode instance` seeded and
  snapshotted (65,201,798 B, 1,294 files, sha256 `b0c8efef…de2177`), `--mode arm` restored it
  (digest verified, 0.5 s), started the container on the restored directory and returned oracle PASS
  (logs `chain-instance-16.15.out`, `chain-arm-restored-16.15.out`). A relative `--instance-root`
  used to land under `harness/.tmp/`; both state paths are resolved once now.

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

## The quiet-box trigger: it was not lowered, and it held

The trigger stays **loadavg under 2 for one minute with no other agent writing in this tree**. At the
chain check's start (20:31) it read 3.77 / 4.03 / 3.40 and no rate was claimed. At 20:32-20:35 it fell
under 2 and stayed: 1.84 at 20:32:35, 1.65, 1.60, 1.40, 1.45, 1.13 at 20:33:50, 0.92 at the measured
arm's start, 0.95 at its end. The 200 rps block above was taken inside that window; the 5-minute and
15-minute averages were still 2.54 / 2.94, which is why the trigger names the 1-minute reading rather
than a smoothed one. `.scratch/89-m3b-paired-200/` and M5's three-arm comparison still need their own
window; nothing in this tree can produce one.

## Decision needed: none

Your answer settled the version question: 16.15 now, 18.6 only if the image lands and the server
reads it back. The postgres arm's own block is measured inside one quiet trigger; what remains open is
the three-arm comparison (sqlite and flashdb-declared in the same window at this version) and the 18.x
arm if the pull lands. Both wait on a quiet box, which no work in this tree can produce.

Effort: ~28 minutes of wall clock, idle class, one background image pull (bandwidth only, no layer
completed), two postgres arms (one 200-request chain check under loadavg 3.77, not scored; one
4,000-request block inside the quiet trigger, scored), ~30 MB of regenerable state under `.tmp/`, no
device held exclusively, no commit pushed.
