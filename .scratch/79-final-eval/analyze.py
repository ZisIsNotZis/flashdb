#!/usr/bin/env python3
"""79-final-eval: extract the decisive-set tables from the raw per-seed JSON.

Read-only analysis; writes nothing. Run from the repo root:
    python3 .scratch/79-final-eval/analyze.py
"""
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
SEEDS = ["42", "43", "44", "42r"]
ARMS = ["declared", "learned@1.0", "static-derived"]


def load(seed, drift=False):
    name = f"{seed}-drift" if drift else seed
    return json.load(open(HERE / f"{name}.json"))


def arm_of(d, wl, arm):
    return d["arms"][f"{wl}/{arm}"]


def curve_table(d, wl):
    print(f"\n### {wl} curve (per rung per arm per seed)")
    hdr = ["seed", "arm", "rung", "achieved", "lag_rej", "svc_rej", "lag_p99_us", "svc_p99_us",
           "err_win", "ach_frac", "sustained"]
    print(" | ".join(hdr))
    for seed in SEEDS:
        d = load(seed)
        for arm in ARMS:
            r = arm_of(d, wl, arm)
            for row in r["curve"]:
                print(" | ".join([
                    seed, arm, str(int(row["offered_rps"])),
                    f"{row['achieved_rps']:.1f}",
                    str(row["lag_ceiling_rejected_windows"]),
                    str(row["service_ceiling_rejected_windows"]),
                    str(row["lag_p99_us"]),
                    str(row["p99_us"]),
                    str(row["error_windows"]),
                    f"{row['achieved_fraction']:.4f}",
                    str(row["sustained"]),
                ]))


def agg_achieved(d, wl):
    print(f"\n### {wl} aggregate achieved_rps (summary) + compact_bytes_total")
    for seed in SEEDS:
        d = load(seed)
        for arm in ARMS:
            r = arm_of(d, wl, arm)
            s = r["summary"]
            print(f"{seed} {arm}: achieved={s.get('achieved_rps')} "
                  f"compact_bytes_total={s.get('compact_bytes_total')} "
                  f"merges={s.get('compact_merges')}")


def index_builds(d, wl):
    print(f"\n### {wl} P|StockMovement index_output_bytes + learned final index flag")
    for seed in SEEDS:
        d = load(seed)
        for arm in ARMS:
            r = arm_of(d, wl, arm)
            reg = r["regions"].get("P|StockMovement", {})
            fp = r.get("final_policy") or {}
            region_pol = fp.get("regions", {}).get("P|StockMovement", {})
            print(f"{seed} {arm}: P|SM index_bytes={reg.get('index_output_bytes', 0)} "
                  f"final_index_order_no={region_pol.get('index_order_no')}")


def region_counts(d, wl):
    print(f"\n### {wl} region counts (records per region, first seed only)")
    d = load(SEEDS[0])
    for arm in ARMS:
        r = arm_of(d, wl, arm)
        counts = {k: v["count"] for k, v in r["regions"].items()}
        print(f"{arm}: {counts}")


def drift_table():
    print("\n### drift (write x10, R=1450, pre=6) per-arm summary + pre/post steady")
    for seed in SEEDS:
        d = load(seed, drift=True)
        for arm in ARMS:
            r = arm_of(d, "drift", arm)
            s = r["summary"]
            ws = r["windows"]
            # boundary = request 12,000 (6 pre segments x 2000)
            straddle = None
            for w in ws:
                if w["completed"] >= 12000:
                    straddle = w["window"]
                    break
            # static/declared: pre steady = mean achieved of pre windows w3..straddle-1
            # learned: pre is a catch-up backlog sum; report the offered 1450 separately
            pre_win = [w for w in ws if w["window"] < straddle and w["window"] >= 3]
            post_win = [w for w in ws if w["window"] > straddle]
            post_steady_win = post_win[:-2] if len(post_win) > 2 else post_win
            pre_steady = (sum(w["achieved_rps"] for w in pre_win) / len(pre_win)
                          if pre_win else None)
            post_steady = (sum(w["achieved_rps"] for w in post_steady_win) / len(post_steady_win)
                           if post_steady_win else None)
            rec = r.get("recovery_windows")
            print(f"{seed} {arm}: achieved={s.get('achieved_rps')} "
                  f"compact_total={s.get('compact_bytes_total')} "
                  f"straddle=w{straddle} n_pre={len(pre_win)} n_post={len(post_steady_win)} "
                  f"pre_steady={pre_steady and round(pre_steady,1)} "
                  f"post_steady={post_steady and round(post_steady,1)} "
                  f"recovery_windows={rec}")


def main():
    for wl in ["win", "adversarial"]:
        curve_table(load(SEEDS[0]), wl)
        agg_achieved(load(SEEDS[0]), wl)
        index_builds(load(SEEDS[0]), wl)
        region_counts(load(SEEDS[0]), wl)
    drift_table()


if __name__ == "__main__":
    main()
