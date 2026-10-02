#!/usr/bin/env bash
# Slice 23 measurement driver: the real inventory workload through the
# deferred-barrier grouped executor at group sizes 1, 8, 64.
#
# Generates a deterministic seed trace + flow trace with the harness modules,
# seeds a fresh engine per group size, then offers the flow trace open-loop at a
# fixed rate via `flashdb-load --group-requests B`. Reproduces the exact
# commands and parameters recorded in .scratch/23-group-commit/issues.md.
#
# usage: scripts/group-commit-run.sh [WORKDIR]
# env:   REQS (flow requests, default 1500), ON_HAND (seeded stock, default 4),
#        RATE (offered rps, default 6400), N_SKU/N_LOC/N_CUSTOMERS.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK=${1:-/tmp/flashdb-group-measure}
REQS=${REQS:-4000}
ON_HAND=${ON_HAND:-4}
RATE=${RATE:-6400}
N_SKU=${N_SKU:-10000}
N_LOC=${N_LOC:-8}
N_CUSTOMERS=${N_CUSTOMERS:-5000}

SCALE="$ROOT/target/release/flashdb-scale"
LOAD="$ROOT/target/release/flashdb-load"
for b in "$SCALE" "$LOAD"; do
  [ -x "$b" ] || { echo "missing $b; run: cargo build --release --offline --bin flashdb-scale --bin flashdb-load" >&2; exit 1; }
done

mkdir -p "$WORK"
REQS="$REQS" ON_HAND="$ON_HAND" N_SKU="$N_SKU" N_LOC="$N_LOC" N_CUSTOMERS="$N_CUSTOMERS" \
python3 - "$ROOT" "$WORK" <<'PY'
import os, sys
root, work = sys.argv[1], sys.argv[2]
sys.path.insert(0, root + "/harness")
from flashdb_harness.seed import SeedGenerator
from flashdb_harness.generator import Generator, render
n_sku = int(os.environ["N_SKU"]); n_loc = int(os.environ["N_LOC"])
n_customers = int(os.environ["N_CUSTOMERS"])
seed = SeedGenerator(seed=42, n_customers=n_customers, n_sku=n_sku, n_loc=n_loc,
                     on_hand=int(os.environ["ON_HAND"]), block_docs=1000)
with open(work + "/seed.jsonl", "w", newline="\n") as f:
    for line in render(seed.blocks()):
        f.write(line + "\n")
gen = Generator(42, int(os.environ["REQS"]), n_customers, n_sku, n_loc)
with open(work + "/flow.jsonl", "w", newline="\n") as f:
    for line in render(gen.requests()):
        f.write(line + "\n")
print(f"generated seed.jsonl and flow.jsonl ({os.environ['REQS']} requests, on_hand={os.environ['ON_HAND']})")
PY

for B in 1 8 64; do
  DIR="$WORK/engine-B$B"
  rm -rf "$DIR"
  "$SCALE" seed-inventory "$DIR" "$WORK/seed.jsonl"
  echo "=== group-requests B=$B offered=$RATE rps ==="
  "$LOAD" "$DIR/data.wal" "$WORK/flow.jsonl" --rate-rps "$RATE" --group-requests "$B" --window-ms 100 \
    | tee "$WORK/load-B$B.out"
done
echo "workdir=$WORK"
