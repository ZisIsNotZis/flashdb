#!/usr/bin/env bash
# 93 — the four-arm chain check for M5's indexed product arm.
#
#   M1_SNAPSHOT=<dir> M1_SOURCE=<dir> bash .scratch/93-indexed-arm/run.sh chain
#   M1_SNAPSHOT=<dir> M1_SOURCE=<dir> bash .scratch/93-indexed-arm/run.sh m5-dry-run
#
# `chain` is a plumbing check, NOT a rate: four arms from the same snapshot, at 50 rps for 5 s,
# under whatever load the box carries. It proves the arm list is accepted, the indexed arm gets its
# `--policy-file`, all four restore/run in one 128 MiB scope at a time, and postgres reads back
# 18.6 with the M1 settings block. It deliberately does NOT run the score step, so no throughput
# number or verdict is produced.
#
# `m5-dry-run` is the same chain at the M1 protocol (5 blocks, rungs 50/100/200, 20 s, cap 128 MiB)
# but still stops before `score`. It is the dry run of the exact M5 invocation written in
# `issues.md`; the score step is the only one not exercised here.
#
# M1_SNAPSHOT points at a run's `snapshot/` directory (ticket 82's is the committed one); the 1.4 GB
# seed trace and 1.6-2.2 GB snapshots are not tracked, so a worktree has to be told where the main
# tree's copies are. M1_SOURCE is the frozen `m1-10x/rung-50` work directory.
set -euo pipefail
cd "$(dirname "$0")/../.."

ACTION=${1:-chain}
: "${M1_SNAPSHOT:?set M1_SNAPSHOT to a snapshot dir (e.g. /home/z/vibe/flashdb/.scratch/82-controlled-m1/runs/snapshot)}"
: "${M1_SOURCE:?set M1_SOURCE to the frozen source dir (e.g. /home/z/vibe/flashdb/.scratch/80-external-baselines/runs/m1-10x/rung-50)}"

ARMS=flashdb-declared,flashdb-indexed,sqlite,postgres
# `m5d` keeps the run root short: postgres' unix socket path has a 107-byte limit and
# `runs/m5-dry-run` would cross it in a worktree this deep (scripts/m1-controlled-run.sh refuses
# such a root up front).
case "$ACTION" in
    chain) ROOT_DEFAULT=.scratch/93-indexed-arm/runs/chain ;;
    m5-dry-run) ROOT_DEFAULT=.scratch/93-indexed-arm/runs/m5d ;;
    *) echo "unknown action $ACTION (expected chain or m5-dry-run)" >&2; exit 2 ;;
esac
ROOT=${M1_RUN_ROOT:-$ROOT_DEFAULT}
COMMON=(
    --root "$ROOT" --source "$M1_SOURCE" --reuse-snapshot "$M1_SNAPSHOT"
    --arms "$ARMS" --cap-mib 128
)

case "$ACTION" in
    chain)
        # One block, one rung, 5 s: the chain, not the protocol.
        bash scripts/m1-controlled-run.sh "${COMMON[@]}" \
            --blocks 1 --rungs 50 --seconds 5 --phase setup
        bash scripts/m1-controlled-run.sh "${COMMON[@]}" \
            --blocks 1 --rungs 50 --seconds 5 --phase measure
        ;;
    m5-dry-run)
        # The M1 protocol parameters, without the score step.
        bash scripts/m1-controlled-run.sh "${COMMON[@]}" \
            --blocks 5 --rungs 50,100,200 --seconds 20 --phase setup
        bash scripts/m1-controlled-run.sh "${COMMON[@]}" \
            --blocks 5 --rungs 50,100,200 --seconds 20 --phase measure
        ;;
esac

echo "93: $ACTION artefacts in $ROOT (no rate claimed; score step not run)"
