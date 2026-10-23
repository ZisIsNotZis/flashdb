#!/usr/bin/env bash
# setup + connection + migration for the postgres arm against the running M5 server.
#
# This is setup, not a measurement: the harness's own seeding path (`prepare` then
# `build-instance --arms postgres`) creates the database, the schema and the dimension rows, and the
# counts and settings are read back from the server. No arm is run, no window is opened, and no
# throughput number is claimed — root's answer of 2026-10-23 puts setup, connection, migration and
# recorded settings on the box-free side of the quiet window.
#
# Usage: smoke-migration.sh [workdir]      (default .tmp/m5-postgres-smoke)
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
WORK=${1:-$REPO/.tmp/m5-postgres-smoke}
PSYCOP='psycopg'  # the 3.3.6 pin pulls psycopg-binary, which is not in this box's uv cache (read at 20:24)

eval "$(bash "$HERE/postgres-server.sh" env)"
echo "# server at $M1_PGHOST:$M1_PGPORT as $M1_PGUSER ($M1_PG_SERVER_VERSION), workdir $WORK"
mkdir -p "$WORK"

harness() { ( cd "$REPO/harness" && uv run --offline --with "$PSYCOP" python -m flashdb_harness.baseline_0b "$@" ); }

echo "# method: uv run --with $PSYCOP python -m flashdb_harness.baseline_0b prepare --workdir $WORK"
harness prepare --workdir "$WORK" --requests 200 >/dev/null
echo "# method: uv run --with $PSYCOP python -m flashdb_harness.baseline_0b build-instance --workdir $WORK --arms postgres"
harness build-instance --workdir "$WORK" --arms postgres --out "$WORK/build-postgres.json"

DB=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["postgres"]["database"])' "$WORK/build-postgres.json")
echo "# database the harness built: $DB (read from $WORK/build-postgres.json)"
echo "# method: docker exec flashdb-m5-pg psql -U postgres -d $DB -tAc '<counts>'"
docker exec flashdb-m5-pg psql -U postgres -d "$DB" -tAc \
  "select 'customers='||count(*) from customer; select 'stocks='||count(*) from stock; select 'database_bytes='||pg_database_size(current_database())"
echo "# method: docker exec flashdb-m5-pg psql -U postgres -tAc 'select version()'"
docker exec flashdb-m5-pg psql -U postgres -tAc "select version()"
echo "smoke: schema, rows and counts read back from the running server"
