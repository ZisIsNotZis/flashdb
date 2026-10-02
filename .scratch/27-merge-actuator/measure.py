#!/usr/bin/env python3
"""Slice 27 measurement: per-region compaction-trigger actuator on the
tile-resident inventory flow. Builds one inventory directory, copies it, and
compacts it under a uniform-eager vs a split policy, then offers the same read
trace to both and reads the per-region telemetry.

Run from the repo root. Prints one machine-readable line per observation.
"""
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness"))
from flashdb_harness.seed import SeedGenerator          # noqa: E402
from flashdb_harness.generator import Generator, render  # noqa: E402

BIN = REPO / "target" / "release"
WD = Path(os.environ.get("SLICE27_DIR", "/tmp/slice27"))
RATE = int(os.environ.get("SLICE27_RATE", "200"))


def run(cmd, env=None, capture=True, check=True):
    e = os.environ.copy()
    if env:
        e.update(env)
    p = subprocess.run([str(c) for c in cmd], cwd=REPO, env=e,
                       capture_output=capture, text=True)
    if check and p.returncode != 0:
        sys.stderr.write(p.stdout or "")
        sys.stderr.write(p.stderr or "")
        raise SystemExit(f"FAILED {cmd}: rc={p.returncode}")
    return p.stdout or ""


def kv(line, key):
    m = re.search(rf"(?:^|\s){re.escape(key)}=(\S+)", line)
    return m.group(1) if m else None


def write_lines(path, lines):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        n = 0
        for line in lines:
            f.write(line + "\n")
            n += 1
    return n


def tileinfo_per_region(dirp):
    out = run([BIN / "flashdb-scale", "tileinfo", dirp])
    regions = {}
    active = memtable = None
    for line in out.splitlines():
        if line.startswith("phase=tileinfo tile="):
            r = kv(line, "region")
            regions[r] = regions.get(r, 0) + 1
        elif "active_tiles=" in line and "serving_memtable_entries=" in line:
            active = int(kv(line, "active_tiles"))
            memtable = int(kv(line, "serving_memtable_entries"))
    return active, memtable, regions


def load_run(dirp, read_trace):
    out = run([BIN / "flashdb-load", dirp, read_trace,
               "--rate-rps", str(RATE), "--window-ms", "1000", "--metrics-json"])
    dev = re.search(r"device_fsync_p50_us=(\S+)", out)
    regions = {}
    for line in out.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        obj = json.loads(line)
        for r in obj["current"]["regions"]:
            # Keep the last window's numbers (steady-ish state) per region.
            regions[r["region"]] = (r["p50_ns"], r["p99_ns"], r["probes"], r["bytes_read"])
    summary = ""
    for line in out.splitlines():
        if line.startswith("load_summary "):
            summary = line
    return dev.group(1) if dev else "none", regions, summary


def main():
    if WD.exists():
        shutil.rmtree(WD)
    WD.mkdir(parents=True)

    # ---- traces ----
    seed = SeedGenerator(seed=85, n_customers=5000, n_sku=2000, n_loc=8,
                         on_hand=1_000_000, block_docs=1000)
    n_seed = write_lines(WD / "seed.jsonl", render(seed.blocks()))
    gen_w = Generator(85, 2000, 5000, 2000, 8)
    n_write = write_lines(WD / "write.jsonl", (json.dumps(r) for r in gen_w.requests()))
    gen_r = Generator(85, 1000, 5000, 2000, 8)
    n_read = write_lines(WD / "read.jsonl", (json.dumps(r) for r in gen_r.requests()))
    print(f"traces seed_blocks={n_seed} write_requests={n_write} read_requests={n_read} rate_rps={RATE}")

    # ---- build the tile-resident inventory directory ----
    base = WD / "base"
    base.mkdir()
    env = {"FLASHDB_REGION_MODE": "per_region", "FLASHDB_SEED_CHECKPOINT_BYTES": "262144"}
    print("seed_inventory:", [l for l in run([BIN / "flashdb-scale", "seed-inventory", base, WD / "seed.jsonl"], env).splitlines() if l.startswith("phase=seed-inventory ok")][-1])
    run([BIN / "flashdb-replay", base, WD / "write.jsonl"], env)
    print("checkpoint:", [l for l in run([BIN / "flashdb-scale", "checkpoint", base], env).splitlines() if l.startswith("phase=checkpoint ok")][-1])
    active, memtable, regions = tileinfo_per_region(base)
    print(f"base active_tiles={active} memtable_entries={memtable} tiles_per_region={regions}")

    # ---- policy files ----
    (WD / "eager.json").write_text(json.dumps({"default": {"merge_min_tiles": 2}}))
    write_only = ["P|Order", "R|Order", "R|StockMovement", "U|StockMovement"]
    split = {"default": {"merge_min_tiles": 2},
             "regions": {r: {"merge_min_tiles": 200} for r in write_only}}
    (WD / "split.json").write_text(json.dumps(split))
    # On this flow the genuinely write-only regions hold one tile each, so a
    # lazy threshold above 1 cannot bite. To show the actuator's real gradient we
    # also defer the regions that actually accumulate multiple tiles (the
    # probe-hot ones, written repeatedly as Stock/Customer are patched): this is
    # the "what deferral buys/costs" demonstration, not the task's eager-for-
    # probe-hot split.
    multi = ["P|Stock", "U|Stock", "P|Customer", "U|Customer"]
    defer = {"default": {"merge_min_tiles": 2},
             "regions": {r: {"merge_min_tiles": 200} for r in multi}}
    (WD / "defer.json").write_text(json.dumps(defer))
    print("split_policy=" + json.dumps(split))
    print("defer_policy=" + json.dumps(defer))

    results = []
    for name in ("eager", "split", "defer"):
        d = WD / name
        shutil.copytree(base, d)
        out = run([BIN / "flashdb-scale", "compact", d],
                  {"FLASHDB_REGION_MODE": "per_region", "FLASHDB_POLICY_FILE": str(WD / f"{name}.json")})
        compact_line = [l for l in out.splitlines() if l.startswith("phase=compact ok")][-1]
        active, memtable, regions = tileinfo_per_region(d)
        dev, rp, summary = load_run(d, WD / "read.jsonl")
        results.append({"name": name, "compact": compact_line, "active": active,
                        "regions": regions, "dev_fsync_p50_us": dev, "load": rp, "summary": summary})
        print(f"{name} compact: {compact_line}")
        print(f"{name} after active_tiles={active} tiles_per_region={regions}")
        print(f"{name} device_fsync_p50_us={dev}")
        for r, (p50, p99, probes, br) in sorted(rp.items()):
            print(f"{name} region={r} probes={probes} p50_ns={p50:.0f} p99_ns={p99:.0f} bytes_read={br}")
        print(f"{name} load_summary: {summary}")

    # ---- comparison / constraint check ----
    print("=== comparison ===")
    eager = next(x for x in results if x["name"] == "eager")
    splitr = next(x for x in results if x["name"] == "split")
    print(f"merges eager={kv(eager['compact'],'merges')} split={kv(splitr['compact'],'merges')}")
    print(f"compaction_bytes eager={kv(eager['compact'],'compaction_bytes')} split={kv(splitr['compact'],'compaction_bytes')}")
    print(f"rewrite_factor eager={kv(eager['compact'],'rewrite_factor')} split={kv(splitr['compact'],'rewrite_factor')}")
    print(f"tile_bytes_before eager={kv(eager['compact'],'tile_bytes_before')} split={kv(splitr['compact'],'tile_bytes_before')}")
    print(f"tile_bytes_after eager={kv(eager['compact'],'tile_bytes_after')} split={kv(splitr['compact'],'tile_bytes_after')}")
    for x in results:
        print(f"{x['name']} merges={kv(x['compact'],'merges')} compaction_bytes={kv(x['compact'],'compaction_bytes')} rewrite_factor={kv(x['compact'],'rewrite_factor')} after_active={x['active']}")
    margin = 0.25
    for r in sorted(set(eager["load"]) | set(splitr["load"])):
        e99 = eager["load"].get(r, (0, 0, 0, 0))[1]
        s99 = splitr["load"].get(r, (0, 0, 0, 0))[1]
        if e99 and s99 > e99 * (1 + margin):
            print(f"CONSTRAINT_VIOLATION region={r} eager_p99={e99} split_p99={s99}")
    print(f"constraint_margin={margin}")


if __name__ == "__main__":
    main()
