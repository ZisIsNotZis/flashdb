"""Run the first runnable inventory workload end to end (Slice 18).

Before this slice the frozen corpora were acceptance fixtures only: replaying them
against a fresh engine reported `cust: empty` / `take: skipped` and the WAL did
not grow, because nothing seeded the Customer and Stock dimensions the flow's
lookups probe. This script makes the workload runnable and measures it:

1. emit a *seeding trace* (Customers + Stock rows, `flashdb_harness.seed`);
2. emit the *flow trace* (`flashdb_harness.generator`'s MIX: order_flow 70,
   stock_lookup 15, customer_lookup 10, movements_scan 5) with the same
   dimensions;
3. `flashdb-scale seed-inventory` applies the seeding trace as raw P/U ops
   (the request adapter has no create shape);
4. `flashdb-replay` replays the flow trace against that seeded WAL and streams
   one JSON result per request;
5. `family_bytes` reports the P/U/R byte shares of the resulting engine, both
   written (every version) and live (newest version per logical key);
6. this script parses the replayer output into block outcomes and prints the
   commit rate and the measured table.

Usage (from the harness directory)::

    uv run python -m flashdb_harness.inventory_workload --requests 60000

It is stdlib-only and also runs as ``python3 flashdb_harness/inventory_workload.py``.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

# Make the package importable when run as a plain script, not only as `-m`.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness.generator import Generator, render  # noqa: E402
from flashdb_harness.seed import SeedGenerator  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    print("+ " + " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run(cmd, **kwargs)


def write_trace(path: Path, lines) -> int:
    with open(path, "w", encoding="utf-8", newline="\n") as out:
        count = 0
        for line in lines:
            out.write(line + "\n")
            count += 1
    return count


def summarize_replay(text: str) -> dict:
    """Parse the replayer's per-request JSONL into block-outcome counters."""
    blocks_seen: Counter[str] = Counter()
    take_status: Counter[str] = Counter()
    cust_status: Counter[str] = Counter()
    read_status: Counter[str] = Counter()
    order_flows = 0
    for line in text.splitlines():
        request = json.loads(line)
        blocks = request["blocks"]
        for block in blocks.values():
            blocks_seen[block["status"]] += 1
        if "take" in blocks:
            order_flows += 1
            take_status[blocks["take"]["status"]] += 1
            if "cust" in blocks:
                cust_status[blocks["cust"]["status"]] += 1
        else:
            read_status[blocks["q"]["status"]] += 1
    committed = take_status.get("ok", 0)
    return {
        "order_flows": order_flows,
        "reads": sum(read_status.values()),
        "read_ok": read_status.get("ok", 0),
        "read_empty": read_status.get("empty", 0),
        "cust": dict(cust_status),
        "take": dict(take_status),
        "blocks": dict(blocks_seen),
        "committed": committed,
        "commit_rate": committed / order_flows if order_flows else 0.0,
    }


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--requests", type=int, default=60_000,
                    help="flow requests to replay (default 60000)")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--customers", type=int, default=5_000)
    ap.add_argument("--skus", type=int, default=10_000)
    ap.add_argument("--locs", type=int, default=8)
    ap.add_argument("--on-hand", type=int, default=1_000_000)
    ap.add_argument("--block-docs", type=int, default=1_000)
    ap.add_argument("--workdir", type=Path, default=None)
    ap.add_argument("--keep", action="store_true", help="keep the temp workdir")
    ap.add_argument("--release", action="store_true", help="build/run the release profile")
    a = ap.parse_args(argv)

    workdir = a.workdir or Path(tempfile.mkdtemp(prefix="inventory-workload-"))
    workdir = workdir.resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    engine_dir = workdir / "engine"
    if engine_dir.exists():
        shutil.rmtree(engine_dir)
    seed_trace = workdir / "seed.jsonl"
    flow_trace = workdir / "flow.jsonl"
    flow_out = workdir / "flow.out.jsonl"

    seed = SeedGenerator(seed=a.seed, n_customers=a.customers, n_sku=a.skus,
                         n_loc=a.locs, on_hand=a.on_hand, block_docs=a.block_docs)
    n_seed_blocks = write_trace(seed_trace, render(seed.blocks()))
    gen = Generator(a.seed, a.requests, a.customers, a.skus, a.locs)
    n_flow = write_trace(flow_trace, render(gen.requests()))
    print(f"workdir={workdir} seed_blocks={n_seed_blocks} flow_requests={n_flow}")

    profile = "release" if a.release else "debug"
    cargo_profile = ["--release"] if a.release else []
    run(["cargo", "build", "--offline", *cargo_profile, "--bin", "flashdb-scale",
         "--bin", "flashdb-replay", "--example", "family_bytes"],
        cwd=REPO_ROOT, check=True)
    bin_dir = REPO_ROOT / "target" / profile
    scale = bin_dir / "flashdb-scale"
    replay = bin_dir / "flashdb-replay"
    family = bin_dir / "examples" / "family_bytes"

    seeded = run([str(scale), "seed-inventory", str(engine_dir), str(seed_trace)],
                 check=True, capture_output=True, text=True)
    for line in seeded.stdout.splitlines():
        print("seed-inventory:", line)

    replayed = run([str(replay), str(engine_dir / "data.wal"), str(flow_trace)],
                   check=True, capture_output=True, text=True)
    flow_out.write_text(replayed.stdout, encoding="utf-8")
    print("replay stderr:", replayed.stderr.strip())

    measured = run([str(family), str(engine_dir)], check=True, capture_output=True, text=True)
    print(measured.stdout, end="")

    # ---- parse the replayer's per-request JSONL into block outcomes ----
    summary = summarize_replay(flow_out.read_text(encoding="utf-8"))

    print()
    print("=== workload report ===")
    print(f"flow_requests       {n_flow}")
    print(f"order_flow requests {summary['order_flows']}")
    print(f"reads               {summary['reads']} "
          f"(ok={summary['read_ok']} empty={summary['read_empty']})")
    print(f"cust block outcomes {summary['cust']}")
    print(f"take block outcomes {summary['take']}")
    print(f"block status counts {summary['blocks']}  "
          f"(conflicted: none -- a unique conflict aborts replay)")
    print(f"committed take blocks {summary['committed']}")
    print(f"commit_rate         {summary['commit_rate']:.4f}")
    if not a.keep:
        shutil.rmtree(workdir)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
