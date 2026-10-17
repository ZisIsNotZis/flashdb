# M1 environment facts (verified 2026-10-17, user `z`, no root)

Docker daemon is up but cannot pull (Docker Hub, PyPI, GitHub unreachable; HTTP to
`mirrors.aliyun.com` and `mirrors.tuna.tsinghua.edu.cn` works). No sudo without a password.
Therefore both baselines run natively; the apt route needs no root for `apt-get download`.

## sqlite

- Available in the Python standard library: `python3 -c "import sqlite3; print(sqlite3.sqlite_version)"`
  -> `3.53.4`. No separate install. Durability is set per connection:
  `PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;`.

## postgres 18.6 (user-space extraction, no root)

Proven sequence (also the script `scripts/baseline-env.sh` must reproduce):

```
cd /tmp/pgprobe
apt-get download postgresql-18 postgresql-client-18 postgresql-common libpq5
mkdir -p root && for f in *.deb; do dpkg-deb -x "$f" root; done
export PGROOT=/tmp/pgprobe/root/usr/lib/postgresql/18
export LD_LIBRARY_PATH=/tmp/pgprobe/root/usr/lib/x86_64-linux-gnu:$PGROOT/lib
export PATH=$PGROOT/bin:$PATH
initdb -D /tmp/pgprobe/data -U z --auth=trust --no-sync
```

`postgresql.conf` additions that make it fit a 128 MiB cgroup:

```
shared_buffers = 16MB
max_connections = 32
work_mem = 4MB
maintenance_work_mem = 16MB
fsync = on
synchronous_commit = on
wal_level = minimal
max_wal_senders = 0
unix_socket_directories = '/tmp/pgprobe'
port = 55432
listen_addresses = ''
```

Verified at a 128 MiB cap:

```
systemd-run --user --scope -p MemoryMax=128M bash -lc \
  "LD_LIBRARY_PATH=$LD_LIBRARY_PATH PATH=$PATH \
   pg_ctl -D /tmp/pgprobe/data -l /tmp/pgprobe/pg.log start && \
   psql -h /tmp/pgprobe -p 55432 -U z -d postgres -c 'select 1 as ok' && \
   psql -h /tmp/pgprobe -p 55432 -U z -d postgres -c 'show shared_buffers' && \
   pg_ctl -D /tmp/pgprobe/data stop"
```

Result: `ok=1`; `shared_buffers=16MB`; clean shutdown. This is the config that makes the
128 MiB regime fair to postgres; it is minimal, not tuned, and must be disclosed verbatim in the
report.

## psycopg

`psycopg` 3.3.6 (binary) is already in the uv cache (`~/.cache/uv/archive-v0/...`), so
`uv run --with psycopg python ...` works offline.

## Caveat to state on every number

The postgres config above is chosen so a shared-buffer-sized engine fits the same 128 MiB cgroup as
flashdb; it is not postgres's default and not a tuning attempt. The sqlite page cache and journal
mode, and the flashdb group-commit batch, must be disclosed on the same terms.
