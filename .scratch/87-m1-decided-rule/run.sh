#!/usr/bin/env bash
# WP-12 — read the saved M1 records (ticket 82) under the decided lag-only rule.
#
# Runs no workload: it reads .scratch/82-controlled-m1/runs/** only, and writes only under this
# ticket. The score script's JSON output goes to /tmp so the committed runs/report.json is not
# overwritten.
set -euo pipefail
cd "$(dirname "$0")/../.."          # repository root

# 1. The 45-row re-derivation, the per-arm per-rung table, and the 10 ms service-ceiling counterfactual.
python3 .scratch/87-m1-decided-rule/analysis.py | tee .scratch/87-m1-decided-rule/logs/analysis.out

# 2. T-84's own claim: the scorer reproduces the committed table.md byte-identically at HEAD.
SINCE=$(sed 's/SINCE=//' .scratch/82-controlled-m1/runs/since.txt)
python3 scripts/m1-controlled-score.py --root .scratch/82-controlled-m1/runs \
    --out /tmp/wp12-score-report.json \
    --markdown .scratch/87-m1-decided-rule/reproduce/table.reproduced.md \
    --since "$SINCE" > .scratch/87-m1-decided-rule/logs/m1-controlled-score.out 2>&1

diff -u .scratch/82-controlled-m1/runs/table.md \
        .scratch/87-m1-decided-rule/reproduce/table.reproduced.md \
  | wc -l | tee .scratch/87-m1-decided-rule/reproduce/table.diff-lines.txt
sha256sum .scratch/82-controlled-m1/runs/table.md \
          .scratch/87-m1-decided-rule/reproduce/table.reproduced.md \
  | tee .scratch/87-m1-decided-rule/reproduce/table.sha256.txt

# 3. The provenance of the readings: was any M1 arm-run ever scored with a service ceiling?
#    (`|| true` because `grep` exits 1 when a pattern has no hits, which is itself a finding)
grep -l '"service_ceiling_us": null' .scratch/82-controlled-m1/runs/b*/rung-*/windows-*.jsonl | wc -l
grep -c '"service_ceiling_us": [0-9]' .scratch/82-controlled-m1/runs/b*/rung-*/windows-*.jsonl | grep -v ':0' | wc -l || true
grep -l "sustained_primary" .scratch/82-controlled-m1/runs/b*/rung-*/run-*.json | wc -l || true
