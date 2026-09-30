#!/usr/bin/env bash
# Scale benchmark runner for flashdb-scale (machine-readable lines on stdout).
#
# usage: scripts/scale-run.sh DIR DOCS COMMIT MEM
#   DIR    engine directory (removed first)
#   DOCS   total documents to seed
#   COMMIT docs per commit (block)
#   MEM    MemoryMax for systemd-run, e.g. 1G
# Env knobs: VALUE_BYTES (default 256), PASSES (default 3),
#            FLASHDB_SCALE_BIN (explicit binary path).
#
# Each phase runs under `systemd-run --user --scope -p MemoryMax=$MEM
# -p MemorySwapMax=0` when available (hard memory cap), else directly. Per
# phase: wall seconds via date +%s, exit code, and RSS peak parsed from the
# binary's rss_kb=/rss_peak_kb= lines. Per-phase stdout/stderr lands in a temp
# log whose path is printed before the phase starts, so a supervisor can tail
# progress lines live.
set -u

if [ $# -ne 4 ]; then
  echo "usage: $0 DIR DOCS COMMIT MEM" >&2
  exit 2
fi
DIR=$1
DOCS=$2
COMMIT=$3
MEM=$4
VALUE_BYTES=${VALUE_BYTES:-256}
PASSES=${PASSES:-3}
case "$DIR" in
  ""|"/"|"."|"..") echo "refusing to operate on '$DIR'" >&2; exit 2 ;;
esac

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN=${FLASHDB_SCALE_BIN:-}
if [ -z "$BIN" ]; then
  for cand in "$ROOT/target/release/flashdb-scale" "$ROOT/target/debug/flashdb-scale"; do
    if [ -x "$cand" ]; then BIN=$cand; break; fi
  done
fi
if [ -z "$BIN" ]; then
  echo "phase=build status=running"
  if (cd "$ROOT" && cargo build --release --offline); then
    BIN=$ROOT/target/release/flashdb-scale
  else
    echo "phase=build ok=false" >&2
    exit 1
  fi
fi
echo "bin=$BIN"

WRAP=()
if command -v systemd-run >/dev/null 2>&1 &&
   systemd-run --user --scope -p "MemoryMax=$MEM" -p MemorySwapMax=0 true >/dev/null 2>&1; then
  WRAP=(systemd-run --user --scope -p "MemoryMax=$MEM" -p MemorySwapMax=0)
  echo "wrapper=systemd-run mem=$MEM"
else
  echo "wrapper=direct mem=$MEM systemd_run=unavailable"
fi

LOGDIR="$(mktemp -d /tmp/flashdb-scale-XXXXXX)"
echo "log_dir=$LOGDIR value_bytes=$VALUE_BYTES passes=$PASSES"

rm -rf "$DIR"

FAILED=0
run_phase() {
  local name=$1
  shift
  if [ "$FAILED" -ne 0 ]; then
    echo "phase=$name status=skipped"
    return 0
  fi
  local log="$LOGDIR/$name.log" t0 t1 rc peak cgpeak
  echo "phase=$name status=running log=$log"
  t0=$(date +%s)
  # Run the phase through a subshell inside the scope so the cgroup's own accounting
  # (which includes page cache) can be reported next to the process RSS: a MemoryMax cap
  # counts page cache too, so the two views must never be conflated.
  LOG="$log" BIN="$BIN" "${WRAP[@]}" bash -c '
    "$BIN" "$@" > "$LOG" 2>&1
    rc=$?
    cg=$(sed -n "s/^0:://p" /proc/self/cgroup 2>/dev/null)
    if [ -n "$cg" ] && [ -r "/sys/fs/cgroup$cg/memory.peak" ]; then
      echo "cgroup_peak_bytes=$(cat "/sys/fs/cgroup$cg/memory.peak")" >> "$LOG.cgroup"
    fi
    exit $rc
  ' bash "$@"
  rc=$?
  t1=$(date +%s)
  peak=$(grep -hoE 'rss(_peak)?_kb=[0-9]+' "$log" | sed 's/.*=//' | sort -n | tail -1)
  cgpeak=$(sed -n 's/^cgroup_peak_bytes=//p' "$log.cgroup" 2>/dev/null | tail -1)
  echo "phase=$name exit=$rc elapsed_s=$((t1 - t0)) rss_peak_kb=${peak:-na} cgroup_peak_bytes=${cgpeak:-na} log=$log"
  if [ "$rc" -ne 0 ]; then
    echo "phase=$name ok=false" >&2
    tail -n 20 "$log" >&2
    FAILED=1
  fi
}

run_phase seed      seed "$DIR" "$DOCS" "$COMMIT" "$VALUE_BYTES"
run_phase verify    verify "$DIR" "$DOCS"
run_phase scanbench scanbench "$DIR" "$PASSES"

if [ "$FAILED" -ne 0 ]; then
  echo "scale_run=failed"
  exit 1
fi
echo "scale_run=ok"
