"""Offered-load profile generator + deterministic drift injection (Slice 20).

Why
---
The driver used to replay a fixed trace as fast as possible. Objective v1
(`.scratch/15-objective-v0/objective-v1.md`) scores a **capacity curve** over
*offered load* levels, with a pre-registered per-pattern latency ceiling as a
constraint. That needs the offered load (a rate per request mix) to be a
first-class input, and drift to be injected at known segment boundaries.

This module emits:

1. the request trace in the existing JSONL dialect (`Generator` + `render`), so
   `engine/src/request.rs`'s adapter accepts every line unchanged; and
2. a **profile manifest** (JSON, sorted keys, LF endings) the Rust pacer
   consumes to schedule the offered load and to attribute windows to segments.

Determinism
-----------
Only the harness `SplitMix64` `Rng` and the integer `Zipf` are used in the data
path: no `random`, no float accumulation. Profile arithmetic (rate x seconds,
the SLO inflation) may use numbers, but the emitted request sequence is
byte-identical for the same seed and dimensions; `tests/test_load_profile.py`
proves it. Drift is a deterministic function of the seed: the injected shape,
the spiked key and every rng draw come from one continuous stream.

The trace stays inside the adapter's accepted dialect. A "hard deadline" is not
a new request template -- the adapter rejects unknown service-class fields -- so
it is declared in the manifest's drift object (`deadline_s`) for the injected
shape; the pacer, which reads the manifest, applies the deadline.

Manifest schema (frozen field names, v1)
----------------------------------------
Top level (JSON object, `sort_keys=True`, LF endings)::

    {
      "schema": "flashdb.load-profile.manifest.v1",
      "seed": <int>,
      "n_customers": <int>, "n_sku": <int>, "n_loc": <int>, "theta": <number>,
      "total_requests": <int>,
      "trace_sha256": "<hex sha256 of the emitted trace bytes>",
      "segments": [ <segment>, ... ]
    }

Segment object::

    {
      "index": <int>,            # 0-based position in the profile
      "start_request": <int>,    # 0-based line offset into the trace
      "count": <int>,            # requests in this segment
      "offered_rate": <int>,     # requests per second offered for those `count` requests
      "mix": { "<shape>": <int weight>, ... },   # effective mix (integers, MIX shape names)
      "drift": <drift object | null>
    }

Drift object (fields present only for their kind, plus `kind`/`shape`/`description`)::

    {
      "kind": "weight" | "key" | "deadline",
      "shape": "<shape name>",   # weight: multiplied; deadline: added; key: whose sku draws spike
      "factor": <int>,           # kind == "weight": mix[shape] *= factor
      "sku_rank": <int>,         # kind == "key": 1-based Zipf rank to spike
      "spike_pct": <int>,        # kind == "key": percent of that shape's sku draws replaced
      "weight": <int>,           # kind == "deadline": mix[shape] += weight (low, but hard-deadline)
      "deadline_s": <number>,    # kind == "deadline": the hard deadline
      "description": "<human-readable summary>"
    }

The pacer derives the reference (lowest) load level with `reference_segment_index`
and the SLO ceiling per pattern with `slo_ceilings` over a reference-load metrics
table (p99 x `DEFAULT_SLO_INFLATION`).

Usage (from the harness directory)::

    # built-in example profile
    uv run python -m flashdb_harness.load_profile \
        --out-trace trace.jsonl --out-manifest trace.manifest.json --report

    # explicit spec
    uv run python -m flashdb_harness.load_profile --spec profile.json \
        --out-trace trace.jsonl --out-manifest trace.manifest.json
"""

from __future__ import annotations

import argparse
import hashlib
import io
import itertools
import json
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import IO, Iterator, Mapping, Sequence

from flashdb_harness.generator import MIX, Generator, pick_kind, render, sku_name

MANIFEST_SCHEMA = "flashdb.load-profile.manifest.v1"
DRIFT_KINDS = ("weight", "key", "deadline")
# Objective v1: the SLO ceiling is 2x the reference-load p99, declared per pattern.
DEFAULT_SLO_INFLATION = 2.0


@dataclass(frozen=True)
class Drift:
    """One segment's drift injection. The injected `shape` is always explicit."""

    kind: str
    shape: str
    factor: int | None = None
    sku_rank: int | None = None
    spike_pct: int | None = None
    weight: int | None = None
    deadline_s: float | None = None

    def __post_init__(self) -> None:
        if self.kind not in DRIFT_KINDS:
            raise ValueError(f"unknown drift kind {self.kind!r}; expected one of {DRIFT_KINDS}")
        if self.shape not in MIX:
            raise ValueError(f"unknown drift shape {self.shape!r}; expected one of {tuple(MIX)}")
        if self.kind == "weight" and (self.factor is None or self.factor < 1):
            raise ValueError("weight drift requires factor >= 1")
        if self.kind == "key":
            if self.sku_rank is None or self.sku_rank < 1:
                raise ValueError("key drift requires sku_rank >= 1")
            if self.spike_pct is None or not 0 < self.spike_pct <= 100:
                raise ValueError("key drift requires 0 < spike_pct <= 100")
        if self.kind == "deadline" and (self.weight is None or self.weight < 1):
            raise ValueError("deadline drift requires weight >= 1")

    def describe(self) -> str:
        if self.kind == "weight":
            return f"multiply {self.shape} weight x{self.factor}"
        if self.kind == "key":
            return f"spike sku rank {self.sku_rank} in {self.spike_pct}% of {self.shape} sku draws"
        return f"add {self.shape} at weight {self.weight} with a {self.deadline_s}s hard deadline"

    def to_manifest(self) -> dict:
        out: dict = {"kind": self.kind, "shape": self.shape, "description": self.describe()}
        if self.kind == "weight":
            out["factor"] = self.factor
        elif self.kind == "key":
            out["sku_rank"] = self.sku_rank
            out["spike_pct"] = self.spike_pct
        elif self.kind == "deadline":
            out["weight"] = self.weight
            out["deadline_s"] = self.deadline_s
        return out


@dataclass(frozen=True)
class Segment:
    """One offered-load level: a rate, a duration (or count), a mix, optional drift.

    Exactly one of `seconds` / `count` must be given; `offered_rate` is always the
    offered requests-per-second. `mix` defaults to the generator's `MIX`; when given
    it replaces the base mix (shapes absent from it get weight 0).
    """

    offered_rate: int
    seconds: int | None = None
    count: int | None = None
    mix: tuple[tuple[str, int], ...] | None = None
    drift: Drift | None = None

    def __post_init__(self) -> None:
        if self.offered_rate <= 0:
            raise ValueError(f"offered_rate must be > 0, got {self.offered_rate}")
        if (self.seconds is None) == (self.count is None):
            raise ValueError("exactly one of seconds/count must be set")
        if self.seconds is not None and self.seconds <= 0:
            raise ValueError(f"seconds must be > 0, got {self.seconds}")
        if self.count is not None and self.count <= 0:
            raise ValueError(f"count must be > 0, got {self.count}")
        if self.mix is not None:
            for shape, weight in self.mix:
                if shape not in MIX:
                    raise ValueError(f"unknown mix shape {shape!r}; expected one of {tuple(MIX)}")
                if weight <= 0:
                    raise ValueError(f"mix weight for {shape!r} must be > 0, got {weight}")

    def segment_count(self) -> int:
        return self.count if self.count is not None else self.offered_rate * self.seconds

    def effective_mix(self) -> dict[str, int]:
        """The mix actually used, in canonical `MIX` order, after applying drift."""
        base = dict(MIX) if self.mix is None else dict(self.mix)
        if self.drift is not None:
            if self.drift.kind == "weight":
                base[self.drift.shape] = base.get(self.drift.shape, 0) * self.drift.factor
            elif self.drift.kind == "deadline":
                base[self.drift.shape] = base.get(self.drift.shape, 0) + self.drift.weight
        return {shape: base[shape] for shape in MIX if base.get(shape, 0) > 0}


@dataclass
class LoadProfile:
    segments: list[Segment]
    seed: int = 1
    n_customers: int = 5_000
    n_sku: int = 10_000
    n_loc: int = 8
    theta: float = 1.0

    def __post_init__(self) -> None:
        if not self.segments:
            raise ValueError("a profile needs at least one segment")

    def total_requests(self) -> int:
        return sum(seg.segment_count() for seg in self.segments)


# ---------------------------------------------------------------------------
# Trace emission (the determinism-critical path: integer rng, no floats)
# ---------------------------------------------------------------------------
def iter_segment_requests(profile: LoadProfile) -> Iterator[tuple[int, str, dict]]:
    """Yield `(segment_index, shape, request)`; one continuous seed-driven stream."""
    gen = Generator(
        profile.seed,
        profile.total_requests(),
        profile.n_customers,
        profile.n_sku,
        profile.n_loc,
        profile.theta,
    )
    for index, seg in enumerate(profile.segments):
        key_drift = seg.drift if (seg.drift is not None and seg.drift.kind == "key") else None
        if key_drift is None:
            gen.set_key_spike(None, 0)
        mix = seg.effective_mix()
        cum = list(itertools.accumulate(mix[shape] for shape in mix))
        total_weight = cum[-1]
        for _ in range(seg.segment_count()):
            kind = pick_kind(gen.rng, mix, cum, total_weight)
            if key_drift is not None and kind == key_drift.shape:
                gen.set_key_spike(key_drift.sku_rank, key_drift.spike_pct)
            elif key_drift is not None:
                gen.set_key_spike(None, 0)
            yield index, kind, gen.shape(kind)


def write_trace(profile: LoadProfile, out: IO[bytes]) -> tuple[int, str]:
    """Write the trace bytes to `out`; return `(line_count, sha256_hex)`."""
    digest = hashlib.sha256()
    count = 0
    for _index, _kind, req in iter_segment_requests(profile):
        data = (next(render([req])) + "\n").encode("utf-8")
        out.write(data)
        digest.update(data)
        count += 1
    return count, digest.hexdigest()


def trace_bytes(profile: LoadProfile) -> bytes:
    buf = io.BytesIO()
    write_trace(profile, buf)
    return buf.getvalue()


def build_manifest(profile: LoadProfile, trace_sha256: str) -> dict:
    """The manifest the pacer consumes; segment boundaries are cumulative counts."""
    segments = []
    start = 0
    for index, seg in enumerate(profile.segments):
        count = seg.segment_count()
        segments.append({
            "index": index,
            "start_request": start,
            "count": count,
            "offered_rate": seg.offered_rate,
            "mix": seg.effective_mix(),
            "drift": seg.drift.to_manifest() if seg.drift is not None else None,
        })
        start += count
    return {
        "schema": MANIFEST_SCHEMA,
        "seed": profile.seed,
        "n_customers": profile.n_customers,
        "n_sku": profile.n_sku,
        "n_loc": profile.n_loc,
        "theta": profile.theta,
        "total_requests": start,
        "trace_sha256": trace_sha256,
        "segments": segments,
    }


def generate(profile: LoadProfile) -> tuple[bytes, dict]:
    """Return `(trace_bytes, manifest)` in one pass over the sequence."""
    buf = io.BytesIO()
    count, digest = write_trace(profile, buf)
    manifest = build_manifest(profile, digest)
    assert manifest["total_requests"] == count
    return buf.getvalue(), manifest


def manifest_text(manifest: dict) -> str:
    """Canonical manifest serialization: sorted keys, LF endings."""
    return json.dumps(manifest, sort_keys=True, indent=2, ensure_ascii=False) + "\n"


def classify(request: dict) -> str:
    """Recover the shape from an emitted request, as the adapter would see it."""
    blocks = request["blocks"]
    if "take" in blocks:
        return "order_flow"
    query = blocks["q"]
    if "get" in query:
        return "stock_lookup"
    entity = next(iter(query["find"]))
    if entity == "Customer":
        return "customer_lookup"
    if entity == "StockMovement":
        return "movements_scan"
    raise ValueError(f"unclassifiable request {request['id']!r}")


def segment_shares(profile: LoadProfile) -> list[dict[str, float]]:
    """Measured per-segment shape shares, classified from the emitted requests."""
    counters = [Counter() for _ in profile.segments]
    for index, _kind, req in iter_segment_requests(profile):
        counters[index][classify(req)] += 1
    shares = []
    for counter in counters:
        total = sum(counter.values())
        shares.append({shape: n / total for shape, n in counter.items()} if total else {})
    return shares


# ---------------------------------------------------------------------------
# SLO ceiling helper (pure function over a reference-load metrics table)
# ---------------------------------------------------------------------------
def reference_segment_index(profile: LoadProfile) -> int:
    """Index of the lowest offered-load level, the reference for the SLO anchor."""
    return min(range(len(profile.segments)), key=lambda i: profile.segments[i].offered_rate)


def slo_ceilings(
    reference_metrics: Sequence[Mapping[str, object]],
    inflation: float = DEFAULT_SLO_INFLATION,
) -> dict[str, float]:
    """Per-pattern latency ceiling = reference-load p99 x `inflation` (2x default).

    `reference_metrics` is a table of rows, each `{"pattern": <name>, "p99": <value>}`,
    measured by replaying the profile at its reference (lowest) load level. Pure over
    the input table, so it is testable without running an engine.
    """
    ceilings: dict[str, float] = {}
    for row in reference_metrics:
        pattern = row.get("pattern")
        p99 = row.get("p99")
        if not isinstance(pattern, str) or isinstance(p99, bool) or not isinstance(p99, (int, float)):
            raise ValueError(f"metrics row needs a string pattern and numeric p99, got {row!r}")
        ceilings[pattern] = p99 * inflation
    return ceilings


# ---------------------------------------------------------------------------
# Example profile, spec parsing, and CLI
# ---------------------------------------------------------------------------
def build_example_profile(seed: int = 7) -> LoadProfile:
    """Two baseline segments around one that injects a 10x forecast query."""
    return LoadProfile(
        seed=seed,
        segments=[
            Segment(offered_rate=1_000, seconds=2),
            Segment(offered_rate=1_000, seconds=2,
                    drift=Drift(kind="weight", shape="movements_scan", factor=10)),
            Segment(offered_rate=2_000, seconds=1),
        ],
    )


def _drift_from_spec(spec: dict) -> Drift:
    return Drift(
        kind=spec["kind"],
        shape=spec["shape"],
        factor=spec.get("factor"),
        sku_rank=spec.get("sku_rank"),
        spike_pct=spec.get("spike_pct"),
        weight=spec.get("weight"),
        deadline_s=spec.get("deadline_s"),
    )


def profile_from_spec(spec: Mapping) -> LoadProfile:
    """Build a `LoadProfile` from a parsed JSON spec (see the module docstring)."""
    segments = []
    for seg in spec["segments"]:
        mix = seg.get("mix")
        drift = seg.get("drift")
        segments.append(Segment(
            offered_rate=seg["offered_rate"],
            seconds=seg.get("seconds"),
            count=seg.get("count"),
            mix=tuple((k, mix[k]) for k in sorted(mix)) if mix is not None else None,
            drift=_drift_from_spec(drift) if drift is not None else None,
        ))
    return LoadProfile(
        segments=segments,
        seed=spec.get("seed", 1),
        n_customers=spec.get("n_customers", 5_000),
        n_sku=spec.get("n_sku", 10_000),
        n_loc=spec.get("n_loc", 8),
        theta=spec.get("theta", 1.0),
    )


def print_report(profile: LoadProfile, manifest: dict) -> None:
    """Segment boundaries, offered rates, and the *measured* injected-shape share."""
    shares = segment_shares(profile)
    print(f"seed={profile.seed} segments={len(profile.segments)} "
          f"total_requests={manifest['total_requests']} trace_sha256={manifest['trace_sha256']}")
    print(f"reference_segment={reference_segment_index(profile)} (lowest offered_rate)")
    header = f"{'idx':>3} {'start':>7} {'count':>7} {'rate/s':>7}  drift"
    print(header)
    for seg, share in zip(manifest["segments"], shares):
        drift = seg["drift"]["description"] if seg["drift"] else "-"
        mix = ",".join(f"{k}:{v}" for k, v in seg["mix"].items())
        print(f"{seg['index']:>3} {seg['start_request']:>7} {seg['count']:>7} "
              f"{seg['offered_rate']:>7}  {drift}")
        print(f"      mix={mix}")
        print("      measured=" + ",".join(f"{k}:{v:.3f}" for k, v in sorted(share.items())))


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="emit an offered-load trace and its profile manifest")
    ap.add_argument("--seed", type=int, default=1, help="used when --spec has no seed")
    ap.add_argument("--spec", type=Path, default=None,
                    help="JSON profile spec; omit to use the built-in example")
    ap.add_argument("--out-trace", default="-")
    ap.add_argument("--out-manifest", default=None)
    ap.add_argument("--report", action="store_true", help="print segments and measured shares")
    a = ap.parse_args(argv)

    if a.spec is not None:
        profile = profile_from_spec(json.loads(a.spec.read_text(encoding="utf-8")))
    else:
        profile = build_example_profile(a.seed)
    data, manifest = generate(profile)

    if a.out_trace == "-":
        sys.stdout.buffer.write(data)
    else:
        Path(a.out_trace).write_bytes(data)
    if a.out_manifest is not None:
        with open(a.out_manifest, "w", encoding="utf-8", newline="\n") as out:
            out.write(manifest_text(manifest))
    if a.report or a.out_manifest is None:
        print_report(profile, manifest)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
