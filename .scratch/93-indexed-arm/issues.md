# 93 — M5's indexed product arm runs in the controlled chain

Contract: WP-16 in `docs/0_ROADMAP.md` §M5 (dispatched 2026-10-27). The M1 loss belonged to the
**declared control** (its policy requested no derived index, `regions:{}`), not to the product arm.
M5's product arm is therefore the same engine and the same pre-load instance **plus** the one
policy file the baselines' secondary access path corresponds to. This ticket makes the controlled
chain able to run it beside the control on the same snapshot and trace, and records what the chain
proves — a chain check, **not a rate**.

## The arm, and the one difference

| arm | instance / snapshot | driver policy |
|---|---|---|
| `flashdb-declared` (control) | `snapshot/flashdb-declared` | none (declared default) |
| `flashdb-indexed` (product) | **the same** `snapshot/flashdb-declared` | `--policy-file .scratch/89-m3b-paired-200/policy/indexed.policy.json` |

The policy is ticket 89's M3(b) file, not a second copy: sha256
`649c591a17933ac2c591da50d2af939268abbcd24f5f2e68df438f0e6600a13a`, bytes
`{"regions":{"P|StockMovement":{"index_order_no":true}}}`. The harness aliases
`flashdb-indexed` to the declared **instance** (`FLASHDB_INSTANCE_ALIASES`): the policy changes no
seeded byte (the index tile is written while serving), so there is one 1.6 GiB snapshot and one
seed, and both arms restore the same bytes (verified digest, below). The only flags that differ
between the two arm runs are the policy file — window, group, lag ceiling, cap, trace and
durability are the same command line.

## What changed (no engine, no scoring-rule change)

- `harness/flashdb_harness/baseline_0b.py`: `flashdb-indexed` is an arm; its instance/snapshot is
  the declared one; `flashdb_load_command`/`run_flashdb_load`/the `arm` subcommand accept
  `--policy-file`; the run record carries `policy`, `policy_file`, `policy_requested`,
  `engine_policy` (read back from the driver) and an `index` block (see below).
- `scripts/m1-baseline-run.sh`: accepts and runs `flashdb-indexed`, passing the policy file only
  for that arm; `--policy-file PATH` overrides the default (ticket 89's file); the path is made
  absolute before the payload `cd`s into `harness/`; a missing file stops the run. The declared
  control passes no policy; sqlite/postgres are untouched.
- `scripts/m1-controlled-run.sh`: forwards `--policy-file`; builds/snapshots **one** flashdb
  instance (`flashdb-indexed` de-duplicates to `flashdb-declared`); `--reuse-snapshot DIR` points
  the chain at an existing `snapshot/` (the committed M1 one) instead of a 7-12 minute re-seed;
  an M1_PG_ROOT whose unix socket path exceeds postgres' 107-byte limit is refused up front.

## The index record: what the engine already emits, and what the chain observed

`run-flashdb-indexed.json` → `index` carries:

- `engine_policy`: `{"regions":{"P|StockMovement":{"index_order_no":true,...}}}` — read back from
  the **driver**, not the file;
- `startup_warm`: `scan_entries`/`index_entries`;
- every `order_no_index` decision line (`trigger`, `covered_rows`, `stale_rows`, `inline_bytes`,
  `outcome`, plus `raw`);
- the `P|StockMovement` region counters summed over the emitted windows (`probes`,
  `probe_misses`, `count`, `bytes_read`, `index_output_bytes`) and `index_scan_us`;
- `index_tile_bytes`: the size of the `order-no-index.tile` sidecar the builds left in the
  instance;
- `scan_path`: one sentence stating what the evidence does and does not show.

Chain check (`b1/rung-50`, 250 requests):

| arm | policy requested | engine policy read back | decisions | `index_tile_bytes` | `index_output_bytes` | region `count` |
|---|---|---:|---:|---:|---:|---:|
| declared | no | `regions:{}` | none | absent | 0 | 7 |
| indexed | yes | `index_order_no:true` on `P\|StockMovement` | `scan_demand/adopted` ×2 (`covered_rows` 36, 74) | 4125 B | 0 | 7 |

**Why `index_output_bytes` is 0 while the index was active:** the eager base build runs before the
first metrics rotation and an interval delta writes no device bytes, so in a 5 s chain nothing is
charged inside an *emitted* window; the 4125-byte sidecar is the device-side evidence the build
happened. A `trigger=scan_demand` line is the driver's own evidence that
`Engine::order_no_index_lookups` advanced — i.e. the movement read path (`request.rs`'s
`lookup_movement_by_order_no`, then the cross-checked hit) consulted the index. The driver emits
**no per-scan index-hit counter**; the record says so instead of inventing one. A 20 s rung is
expected to charge base/merge writes in-window; that is a reading M5 takes, not this ticket.

## The four-arm chain check (a chain check, not a rate)

Command (also `.scratch/93-indexed-arm/run.sh chain`):

```sh
M1_SNAPSHOT=/home/z/vibe/flashdb/.scratch/82-controlled-m1/runs/snapshot \
M1_SOURCE=/home/z/vibe/flashdb/.scratch/80-external-baselines/runs/m1-10x/rung-50 \
bash .scratch/93-indexed-arm/run.sh chain
```

which is `scripts/m1-controlled-run.sh --root .scratch/93-indexed-arm/runs/chain
--source "$M1_SOURCE" --reuse-snapshot "$M1_SNAPSHOT" --blocks 1 --rungs 50 --seconds 5
--arms flashdb-declared,flashdb-indexed,sqlite,postgres --cap-mib 128
--policy-file .scratch/89-m3b-paired-200/policy/indexed.policy.json --phase setup`, then
`--phase measure`. The score step is deliberately not run.

Result (`logs/chain-check.out`, line 1 = loadavg at start `2.91 5.93 8.27`; exit `CHAIN_RC=0`):

- all four arms accepted and ran, one at a time in the 128 MiB scope; records in
  `runs/chain/b1/rung-50/run-<arm>.json`;
- the two flashdb arms restored **the same** bytes:
  `67b3d10c…acd37ac`, 1,683,012,213 B; sqlite `46c6f1e1…1367130` (541,876,224 B), postgres
  `7f9a4ea8…ad1b92` (2,201,362,806 B);
- postgres 18.6 with the M1 record's settings, read back from the running server
  (`runs/chain/b1/rung-50/postgres-server.jsonl`):
  `PostgreSQL 18.6 (Ubuntu 18.6-3build2)`, `shared_buffers=16MB`, `max_connections=32`,
  `work_mem=4MB`, `maintenance_work_mem=16MB`, `fsync=on`, `synchronous_commit=on`,
  `wal_level=minimal`, `max_wal_senders=0`, `settings_checked:true`;
- sqlite and postgres inventory oracles **PASS**; the flashdb read-backs are `DEFERRED` by the
  controlled protocol (`--skip-verify`, the `verify` step's job);
- every arm's `verdict.sustained` is true at the unscored 50 rps chain rung. **No rate is
  claimed**: the box was loaded (loadavg 2.9 → 1.7 through the run), the run is 5 s per arm, and
  the score step did not run.

## The exact M5 invocation (one command, M1 protocol)

Run on a quiet box (the M3(b) trigger: loadavg under 2 for one minute), from the main tree after
merge:

```sh
bash scripts/m1-controlled-run.sh \
  --root .scratch/93-indexed-arm/runs/m5 \
  --source .scratch/80-external-baselines/runs/m1-10x/rung-50 \
  --reuse-snapshot .scratch/82-controlled-m1/runs/snapshot \
  --blocks 5 --rungs 50,100,200 --seconds 20 \
  --arms flashdb-declared,flashdb-indexed,sqlite,postgres \
  --cap-mib 128 --seed-cap-mib 512 \
  --policy-file .scratch/89-m3b-paired-200/policy/indexed.policy.json \
  --phase all
```

Protocol parameters, kept identical to M1 for every arm: **5 blocks**, rungs **50/100/200**,
**20 s** per arm-run (requests = 20 × rung), engine cap **128 MiB** with swap off, the same frozen
trace per (block, rung), the same snapshot (`--reuse-snapshot` restores it with a digest check
before every arm), postgres required at **18.6** with the M1 settings block, one process at a time
under `flock /tmp/m1-measure.lock`, arm order rotated per (block, rung). `--phase all` is the one
command: setup → device floor → measure → `scripts/m1-controlled-score.py`.

**What the dry run did:** the chain check above is the dry run of this command's chain. It ran the
same script, arms, snapshot, policy, cap and lock; it reduced `--blocks`/`--rungs`/`--seconds` to
1/50/5 and stopped before `score`, because a scored comparison under load is exactly what M5 may
not take. The 5-block × 3-rung × 4-arm sweep and the score step are therefore **not exercised
here**; their arguments are the defaults/values above, `bash -n` is clean, the exact argv parses
(the script stops only at a deliberately invalid `--phase`), and the run-spec records the chain's
parameters (`runs/chain/run-spec.json`).

## Suites

- Rust, pinned mirror and offline (`logs/workspace-pinned-and-build.out`, line 1 loadavg
  `8.99 8.76 9.11`): `cargo --config 'source.crates-io.replace-with="aliyun"' --config
  'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' test --workspace
  --offline` → **345 passed, 0 failed** (`TEST_RC=0`), then the three release binaries built
  (`BUILD_RC=0`).
- Harness (`logs/harness.out`, line 1 loadavg
  `1.41 3.21 6.53`): `cd harness && env -u VIRTUAL_ENV uv run --with pytest pytest -q` →
  **210 passed**.

## Limits and what is not verified

- **No rate, no verdict.** This ticket claims none; the score step was not run.
- The scorer (`scripts/m1-controlled-score.py`) still names the declared arm as the product arm
  (`DECLARED_ARM`, T5/T6/T7). Running `--phase all` will therefore write the *declared* verdict
  with the indexed arm merely present in the table. Giving M5 a product-arm verdict is a scoring
  rule change, which WP-16's scope excludes.
- The chain reused ticket 82's committed snapshot (rights: read-only) because the 1.4 GB seed
  trace and 1.6-2.2 GB snapshots are untracked and absent from this worktree; `--reuse-snapshot`
  copies the metadata and symlinks the data, and each restore still verifies the digest.
- The 107-byte unix-socket limit bit once: a `chain-final` run root (113 bytes) made postgres fail
  to start. `m1-controlled-run.sh` now refuses such a root before restoring anything; the committed
  `runs/m5` and `runs/chain` roots are inside the limit (104/107 bytes in this worktree, shorter in
  the main tree).
- Flashdb read-backs in the chain are `DEFERRED` (controlled protocol); only the SQL oracles ran.
