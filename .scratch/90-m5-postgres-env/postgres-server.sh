#!/usr/bin/env bash
# The M5 postgres leg's server: a container from an image already on this box or pulled from the
# configured registry mirror. No system package is installed; `apt-get download` is not used here.
#
# Why a container: root's answer of 2026-10-23 (mail/2026-10-23-answer-postgres-present.md) reads the
# box as carrying `postgres:16` (16.15, container `jolly_lamarr`, port 5432) and no 18.6, and
# sanctions `docker pull postgres:<n>` on port 5433 for the major version ticket 82 measured (18.6).
# `scripts/baseline-env.sh`'s user-space apt extraction stays as the M1 record's route; this script is
# the M5 route.
#
# The configuration is the M1 record's block, byte for byte (scripts/baseline-env.sh:86-101), so the
# re-run changes the engine revision and not the baseline's settings. Every value below is CHECKED
# against the running server by `check`; the version is read back from `select version()`, never from
# the image tag.
#
# Usage:
#   postgres-server.sh up       # pull if absent, run the container, wait until it answers
#   postgres-server.sh down     # stop and remove the container; the data directory stays on disk
#   postgres-server.sh check    # read the version and every setting back from the running server
#   postgres-server.sh env      # print `export ...` lines for the harness (open-items.md row PLAN-postgres-absent)
#   postgres-server.sh status   # one line: container state, image id, version read back
#
# Environment:
#   M5_PG_IMAGE      default postgres:18   (the major version M1 measured: 18.6)
#   M5_PG_REQUIRE    optional, e.g. 18.6   `up` and `check` fail unless the server reports it
#   M5_PG_NAME       default flashdb-m5-pg
#   M5_PG_PORT       default 5433          (host side; the container listens on 5432)
#   M5_PG_ROOT       default /var/tmp/m5-pg  ext4, the same mount as the M1 cluster's default
#   M5_PG_MEM        default 128m          the cgroup the M1 arms ran under
set -euo pipefail

IMAGE=${M5_PG_IMAGE:-postgres:18}
NAME=${M5_PG_NAME:-flashdb-m5-pg}
PORT=${M5_PG_PORT:-5433}
ROOT=${M5_PG_ROOT:-/var/tmp/m5-pg}
MEM=${M5_PG_MEM:-128m}
REQUIRE=${M5_PG_REQUIRE:-}
USER_NAME=postgres
DATABASE=${M5_PG_DATABASE:-postgres}

# The M1 record's configuration (scripts/baseline-env.sh:86-101; docs/plan/gap-analysis.md §5.1).
SETTINGS=(
  shared_buffers=16MB
  max_connections=32
  work_mem=4MB
  maintenance_work_mem=16MB
  fsync=on
  synchronous_commit=on
  wal_level=minimal
  max_wal_senders=0
)

die() { printf 'postgres-server: %s\n' "$*" >&2; exit 1; }

container_state() {
  # `docker inspect` on a missing container prints an empty line and exits 1, so the state is read
  # into a word first and defaulted; a bare `|| echo absent` leaves the newline in the value.
  local state
  state=$(docker inspect -f '{{.State.Status}}' "$NAME" 2>/dev/null | tr -d '\n' || true)
  printf '%s' "${state:-absent}"
}

psql_() { docker exec "$NAME" psql -U "$USER_NAME" -d "$DATABASE" -tAc "$1"; }

configured_conf() {
  # The final word on each setting as the server reports it, with its source, one per line.
  psql_ "select name || '=' || setting || ' (' || source || ')'
         from pg_settings where name in ('shared_buffers','max_connections','work_mem',
         'maintenance_work_mem','fsync','synchronous_commit','wal_level','max_wal_senders',
         'port','data_directory') order by name"
}

server_version() {
  psql_ "select version()"
}

want_conf() {
  # `SHOW` is what a client sees; the comparison unit is the same string the server reports.
  local item name want got
  for item in "${SETTINGS[@]}"; do
    name=${item%%=*}; want=${item#*=}
    got=$(psql_ "show $name" | tr -d ' ')
    if [ "$got" != "$want" ]; then
      printf 'mismatch: %s reported %q, the M1 record says %q\n' "$name" "$got" "$want" >&2
      return 1
    fi
  done
}

pull_if_absent() {
  if docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "image: $IMAGE present ($(docker image inspect -f '{{.Id}}' "$IMAGE" | cut -c1-19))"
    return
  fi
  echo "image: $IMAGE absent, pulling (bandwidth only; the daemon's registry mirror is $(docker info -f '{{json .RegistryConfig.Mirrors}}'))"
  docker pull "$IMAGE" >&2 || die "pull failed; record the read time and the error, and use a local image with M5_PG_IMAGE"
}

up() {
  pull_if_absent
  mkdir -p "$ROOT/data"
  case "$(container_state)" in
    running)
      echo "container: $NAME already running on 127.0.0.1:$PORT (data $ROOT)"
      ;;
    absent)
      # `--user $(id -u):$(id -g)`: the official image supports an arbitrary user, and it is what
      # lets the data directory be a host bind mount this account owns (no root, no chown to 999).
      # PGDATA is pinned to the 16-and-earlier path for every image so the bind is the cluster.
      local args=(-d --name "$NAME"
        --user "$(id -u):$(id -g)"
        --memory "$MEM" --memory-swap "$MEM"
        -p "127.0.0.1:$PORT:5432"
        -v "$ROOT/data:/var/lib/postgresql/data"
        -e PGDATA=/var/lib/postgresql/data
        -e POSTGRES_HOST_AUTH_METHOD=trust
        -e POSTGRES_USER="$USER_NAME")
      local item cmd=()
      for item in "${SETTINGS[@]}"; do cmd+=(-c "$item"); done
      docker run "${args[@]}" "$IMAGE" "${cmd[@]}" >/dev/null
      echo "container: started $NAME from $IMAGE, memory cap $MEM, data $ROOT, port 127.0.0.1:$PORT"
      ;;
    *)
      docker start "$NAME" >/dev/null
      echo "container: restarted $NAME (was $(container_state))"
      ;;
  esac
  local i
  for i in $(seq 1 60); do
    docker exec "$NAME" pg_isready -U "$USER_NAME" -d postgres >/dev/null 2>&1 && break
    sleep 1
  done
  docker exec "$NAME" pg_isready -U "$USER_NAME" -d postgres >/dev/null 2>&1 \
    || { docker logs --tail 20 "$NAME" >&2 || true; die "server did not become ready in 60 s"; }
  check
}

down() {
  case "$(container_state)" in
    absent) echo "container: $NAME absent" ;;
    *) docker rm -f "$NAME" >/dev/null; echo "container: $NAME removed (data $ROOT kept)" ;;
  esac
}

check() {
  [ "$(container_state)" = running ] || die "container $NAME is $(container_state), not running"
  local version
  version=$(server_version)
  echo "# read at $(date -Is) on $(hostname), user $(id -un)"
  echo "# method: docker exec $NAME psql -U $USER_NAME -tAc 'select version()'"
  echo "version: $version"
  echo "# method: docker image inspect -f '{{.Id}}' $IMAGE; docker inspect -f '{{.HostConfig.Memory}}' $NAME"
  echo "image:   $IMAGE $(docker image inspect -f '{{.Id}}' "$IMAGE" | cut -c1-19)"
  echo "memory:  $(docker inspect -f '{{.HostConfig.Memory}}' "$NAME") bytes cap on the container"
  echo "# method: docker exec $NAME psql -U $USER_NAME -tAc 'select name, setting, source from pg_settings ...'"
  configured_conf | sed 's/^/setting: /'
  want_conf || die "a setting differs from the M1 record's block; the run record must disclose it, so this refuses"
  if [ -n "$REQUIRE" ]; then
    case "$version" in
      *" $REQUIRE "*) ;;
      *) die "server reports $version, M5_PG_REQUIRE=$REQUIRE; a result against another version names the version it was measured against" ;;
    esac
  fi
  echo "checked: every setting above matches the M1 record's block"
}

env_out() {
  [ "$(container_state)" = running ] || die "container $NAME is $(container_state), not running"
  printf 'export M1_PGHOST=%q\nexport M1_PGPORT=%q\nexport M1_PGUSER=%q\nexport M1_PGDATABASE=%q\n' \
    127.0.0.1 "$PORT" "$USER_NAME" "$DATABASE"
  printf 'export M1_PG_SERVER_VERSION=%q\n' "$(server_version | sed 's/PostgreSQL \([0-9.]*\).*/\1/')"
  printf 'export M5_PG_ROOT=%q\n' "$ROOT"
}

status() {
  printf 'container: %s  image: %s  port: 127.0.0.1:%s  data: %s\n' \
    "$(container_state)" "$IMAGE" "$PORT" "$ROOT"
  if [ "$(container_state)" = running ]; then
    printf 'version:   %s\n' "$(server_version)"
  fi
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  check) check ;;
  env) env_out ;;
  status) status ;;
  *) die "usage: $0 up|down|check|env|status" ;;
esac
