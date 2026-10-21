#!/usr/bin/env bash
# 89 — regenerate what the paired 200 rps block needs, from the committed records alone.
#
#   bash .scratch/89-m3b-paired-200/prepare-instance.sh
#
# Nothing here is a timing reading: this is state that comes back from committed specs
# (`.scratch/80-external-baselines/runs/m1-10x/rung-50/spec.json`,
# `.scratch/82-controlled-m1/runs/b2/rung-200/spec.json`), so it runs under load and the loadavg is
# written into the log anyway.
#
# The two `prepare` steps REWRITE the committed `spec.json` of their workdir (the harness writes the
# argv it saw). The records are restored at the end: the committed spec is the record of the original
# run, not of this regeneration.
set -euo pipefail
cd "$(dirname "$0")/../.."
L=.scratch/89-m3b-paired-200/logs
mkdir -p "$L"
echo "loadavg at start: $(cat /proc/loadavg)  ($(date -Is))" | tee "$L/prepare-instance.out"
H=harness

SEED=.scratch/80-external-baselines/runs/m1-10x/rung-50/seed.jsonl
SEED_SHA=c8c6bac66f37dde72f62ee0982237dd1227dc257fd58281740e035f4f0f4639a
echo "== the 10x seed trace and the rung-50 trace (1.41 GB; sha must match the committed record) ==" \
    | tee -a "$L/prepare-instance.out"
if [ -f "$SEED" ] && [ "$(sha256sum "$SEED" | cut -c1-64)" = "$SEED_SHA" ]; then
    echo "seed trace present and byte-identical to the record's sha (${SEED_SHA:0:8}...): not regenerated" \
        | tee -a "$L/prepare-instance.out"
else
    ( cd "$H" && uv run python -m flashdb_harness.baseline_0b prepare \
        --workdir ../.scratch/80-external-baselines/runs/m1-10x/rung-50 --seed 42 --requests 1000 \
        --rate 50 --customers 200000 --skus 2650000 --locs 4 --batch 64 --on-hand 1000000 ) \
        2>&1 | tee -a "$L/prepare-instance.out"
fi

echo "== the rung-200 4000-request trace, hard-linked to that seed ==" | tee -a "$L/prepare-instance.out"
( cd "$H" && uv run python -m flashdb_harness.baseline_0b prepare \
    --workdir ../.scratch/82-controlled-m1/runs/b2/rung-200 --seed 42 --requests 4000 --rate 200 \
    --customers 200000 --skus 2650000 --locs 4 --theta 1.0 --on-hand 1000000 \
    --seed-trace ../.scratch/80-external-baselines/runs/m1-10x/rung-50/seed.jsonl ) \
    2>&1 | tee -a "$L/prepare-instance.out"

echo "== the pre-load flashdb instance, snapshotted ==" | tee -a "$L/prepare-instance.out"
scripts/m1-baseline-run.sh --mode instance \
    --workdir "$PWD/.scratch/82-controlled-m1/runs/b2/rung-200" \
    --arms flashdb-declared --snapshot-dir "$PWD/.scratch/82-controlled-m1/runs/snapshot" \
    --instance-root "$PWD/.scratch/82-controlled-m1/runs/live" --seed-cap-mib 512 --cap-mib 128 \
    --skip-build 2>&1 | tee -a "$L/prepare-instance.out"

echo "== restoring the committed run records this regeneration rewrote ==" | tee -a "$L/prepare-instance.out"
# Four records the regeneration overwrites: the two run specs, the machine-state and loadavg series
# the sampler appends to, and the snapshot digest — the committed ones are the ORIGINAL run's record,
# so the regenerated snapshot's own record is kept beside this ticket instead.
cp .scratch/82-controlled-m1/runs/snapshot/flashdb-declared.json \
   .scratch/89-m3b-paired-200/instance-record.json 2>/dev/null || true
git checkout -- .scratch/80-external-baselines/runs/m1-10x/rung-50/spec.json \
                .scratch/82-controlled-m1/runs/b2/rung-200/spec.json \
                .scratch/82-controlled-m1/runs/b2/rung-200/loadavg.jsonl \
                .scratch/82-controlled-m1/runs/b2/rung-200/machine-state.jsonl \
                .scratch/82-controlled-m1/runs/snapshot/flashdb-declared.json 2>/dev/null || true
git status --short .scratch/80-external-baselines/runs/m1-10x/rung-50/ \
                  .scratch/82-controlled-m1/runs/b2/rung-200/ | tee -a "$L/prepare-instance.out"
echo "DONE loadavg at end: $(cat /proc/loadavg)  ($(date -Is))" | tee -a "$L/prepare-instance.out"
