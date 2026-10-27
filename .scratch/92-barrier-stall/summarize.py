#!/usr/bin/env python3
"""Summarize one flashdb-load replay log for ticket 92.

Reports, per block, the max `completion_us` group and its phase split
(`barrier_wait_us` + `append_us` + `fsync_us` + `apply_us` + `residual_us`),
the max `barrier_wait_us` group with its completion, the window-10 maxima (the
window the M1 stall landed in), and the WAL file's length/blocks after the run.
Work-per-request counter only: no rate is computed or claimed.
"""
import argparse
import re

FIELDS = ("window", "seq", "requests", "due_dispatch_us", "barrier_wait_us",
          "append_us", "apply_us", "fsync_us", "maintain_us", "residual_us",
          "completion_us")


def parse(log_path):
    groups, stalls, stat = [], [], {}
    with open(log_path, encoding="utf-8", errors="replace") as fh:
        first = fh.readline().strip()
        if not first.startswith("loadavg"):
            raise SystemExit(f"{log_path}: line 1 must state loadavg, got {first!r}")
        for line in fh:
            if line.startswith("group_phase "):
                row = {k: int(v) for k, v in re.findall(r"(\w+)=(-?\d+)", line)}
                groups.append(row)
            elif line.startswith("window_stall "):
                row = {k: int(v) for k, v in re.findall(r"(\w+)=(-?\d+)", line)}
                stalls.append(row)
            elif line.startswith("wal_after "):
                stat = {k: int(v) for k, v in re.findall(r"(\w+)=(-?\d+)", line)}
    return first, groups, stalls, stat


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", required=True)
    ap.add_argument("--block", required=True)
    ap.add_argument("--log", required=True)
    args = ap.parse_args()
    first, groups, stalls, stat = parse(args.log)
    if not groups:
        print(f"block arm={args.arm} b={args.block} EMPTY no group_phase lines")
        return

    # A barrier-owning group is what the queue-wait instrument attributes; a
    # read-only group has no barrier and its whole service time is residual by
    # construction (the driver does not time read execution separately).
    write_groups = [g for g in groups if g["fsync_us"] > 0 or g["append_us"] > 0]

    def split(g):
        return (f"window={g['window']} seq={g['seq']} requests={g['requests']} "
                f"completion_us={g['completion_us']} due_dispatch_us={g['due_dispatch_us']} "
                f"barrier_wait_us={g['barrier_wait_us']} append_us={g['append_us']} "
                f"fsync_us={g['fsync_us']} apply_us={g['apply_us']} "
                f"maintain_us={g['maintain_us']} residual_us={g['residual_us']}")

    max_completion = max(groups, key=lambda g: g["completion_us"])
    max_wait = max(groups, key=lambda g: g["barrier_wait_us"])
    max_write = max(write_groups, key=lambda g: g["completion_us"]) if write_groups else None
    w10 = [g for g in groups if g["window"] == 10]
    w10m = max(w10, key=lambda g: g["completion_us"]) if w10 else None
    w10w = max(w10, key=lambda g: g["barrier_wait_us"]) if w10 else None
    over = sum(1 for g in groups if g["completion_us"] > 100_000)
    stall_over = sum(1 for s in stalls if s.get("stall_completion_us", 0) > 100_000)
    w10_stall = [s for s in stalls if s.get("window") == 10]

    print(f"block arm={args.arm} b={args.block} loadavg={first.split(maxsplit=1)[1]!r} "
          f"groups={len(groups)} write_groups={len(write_groups)} "
          f"max_completion_us={max_completion['completion_us']} "
          f"max_wait_us={max_wait['barrier_wait_us']} "
          f"groups_over_100ms={over} windows_with_max_over_100ms={stall_over} "
          f"wal_len={stat.get('len', '?')} wal_blocks={stat.get('blocks', '?')}")
    print(f"  max_completion: {split(max_completion)}")
    if max_write is not None:
        print(f"  max_completion_write: {split(max_write)}")
    # Queue-backlog attribution for the max-wait group. `group_phase` lines are
    # emitted in FIFO completion order, so the group's immediate predecessors in
    # the log are exactly the barriers the worker ran while it waited: sum their
    # own append+fdatasync. If that reaches the wait, the wait is a queue behind
    # their service time, not a worker that was idle.
    idx = next(i for i, h in enumerate(groups) if h is max_wait)
    prior_service, prior_count = 0, 0
    for h in reversed(groups[:idx]):
        if h["fsync_us"] == 0 and h["append_us"] == 0:
            continue
        prior_service += h["append_us"] + h["fsync_us"]
        prior_count += 1
        if prior_service >= max_wait["barrier_wait_us"]:
            break
    print(f"  max_wait: {split(max_wait)}")
    print(f"  max_wait_backlog: prior_barriers={prior_count} "
          f"prior_append_fsync_us={prior_service} wait_us={max_wait['barrier_wait_us']} "
          f"wait_minus_prior_us={max_wait['barrier_wait_us'] - prior_service}")
    if w10m is not None:
        print(f"  window10_max_completion: {split(w10m)}")
    if w10w is not None:
        print(f"  window10_max_wait: {split(w10w)}")
    for s in w10_stall:
        print(f"  window10_window_stall: stall_completion_us={s.get('stall_completion_us')} "
              f"stall_barrier_wait_us={s.get('stall_barrier_wait_us')} "
              f"stall_append_us={s.get('stall_append_us')} stall_fsync_us={s.get('stall_fsync_us')} "
              f"stall_residual_us={s.get('stall_residual_us')} max_group_us={s.get('max_group_us')}")


if __name__ == "__main__":
    main()
