"""Structural descriptors of the four join-heavy shapes (M2 / ticket 81, WP 2.2).

Why
---
Ticket 29 found that the flat inventory flow produces four roughly independent
access shapes, so every learnable *per-region* axis measured neutral or harmful
on it — but the workloads the design actually motivates (star-schema fan-out,
multi-hop traceability, as-of join, anti-join) were never generated. Ticket 81
adds them (`generator.JOIN_MIX`) and this module measures what the emitted
requests structurally ask for, before any engine run:

- **fan-out width** — how many child rows one parent row's hop reaches;
- **multi-hop depth** — how many dependent reads the longest path chains;
- **dependent-read share** — how much of a request cannot be issued until an
  earlier read returns;
- **co-location potential** — of the dependent hops, how many have a child row
  that would share a region with its parent under a stated co-location key, i.e.
  how much of the cost a region-keyed layout could collapse, versus how much is
  left to per-row placement (the L2 question) or to per-region parameters.

Nothing here executes the engine, and no number is a latency. The method
is a *count over the emitted request and the modelled data set* the request
addresses (`generator.JoinWorld`), so every figure is reproducible from the
committed corpus and the committed model:

    cd harness && uv run python -m flashdb_harness.join_descriptors \
        --seed 42 --per-shape 25 --out ../.scratch/81-join-heavy-workloads/descriptors.json

A descriptor that would need the engine (fan-out against a real movement
history, block reads per hop, bytes read) is **not** reported here; the
executability gap is recorded in the owning ticket instead.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from statistics import median

from flashdb_harness.generator import JOIN_NAMES, Generator, JoinWorld, render

SCHEMA = "flashdb.join-descriptors.v1"

# The co-location key each shape is measured under. A region key materialised on
# this field would place a child row next to its parent row in the same region;
# any hop whose rows do not carry the parent's value cannot be merged by a key.
SHAPE_KEYS = {
    "star_fanout": "sku",
    "multi_hop": "order_no",
    "as_of": "sku",
    "anti_join": "sku",
}

# Candidate single keys tried for `colocation_best_single_key`. The multi-hop
# hypothesis is that no single key serves every hop, so the maximum over a small
# stated set is the honest way to test it.
CANDIDATE_KEYS = ("sku", "order_no", "lot_no", "supplier_no")

READ_SHAPES = ("get", "find")

# What the modelled population assumes, so a real data set can falsify a number.
MODEL_NOTES = (
    "rows selected from generator.JoinWorld: one product per sku, one supplier per "
    "supplier, 1 + rank % 3 SCD-2 versions per sku with a boundary every 10 days, "
    "50 orders/day with 1-5 lines each, 1-8 products per lot, 0-2 returns per order; "
    "supply history is 1 + (rank*37 + loc*11) % 5 movements per (sku, loc) and is "
    "empty for a sku with (rank*11) % 7 == 0 (~1/7 of the catalogue, the modelled "
    "lost sale); movements are supply only (delta > 0)."
)


@dataclass
class Group:
    """One parent binding of a hop: the rows the child predicate reached from it."""

    where: dict
    rows: list[dict]
    sources: list[dict] = field(default_factory=list)


@dataclass
class Hop:
    """One read block of the emitted request, resolved against the model."""

    block: str
    entity: str
    depth: int
    parents: tuple[str, ...]
    limit: int | None
    groups: list[Group]

    @property
    def dependent(self) -> bool:
        return bool(self.parents)

    @property
    def matched(self) -> list[dict]:
        return [row for group in self.groups for row in group.rows]

    def fanout(self) -> list[int]:
        """Per-parent child-row count (already capped by the block's `limit`)."""
        return [len(group.rows) for group in self.groups]


def _referenced(query: dict) -> tuple[str, ...]:
    """Block names whose rows a hop's predicate binds (`$block.field`)."""
    names: list[str] = []

    def walk(value: object) -> None:
        if isinstance(value, str) and value.startswith("$") and "." in value:
            name = value[1:].split(".", 1)[0]
            if name not in names:
                names.append(name)
        elif isinstance(value, dict):
            for item in value.values():
                walk(item)

    walk(query)
    return tuple(names)


def _ref_block(want: object) -> str | None:
    """The block a predicate value binds from, or None for a param/constant."""
    if isinstance(want, str) and want.startswith("$") and "." in want:
        return want[1:].split(".", 1)[0]
    if isinstance(want, dict):
        blocks = {_ref_block(operand) for operand in want.values()}
        blocks.discard(None)
        assert len(blocks) <= 1, f"predicate value binds several blocks: {want!r}"
        return blocks.pop() if blocks else None
    return None


def _fill(want: object, row: dict) -> tuple[object, bool]:
    """Substitute the fields of one parent `row` into a predicate value."""
    if isinstance(want, str) and want.startswith("$") and "." in want:
        field_name = want.split(".", 1)[1]
        return (row.get(field_name), field_name in row)
    if isinstance(want, dict):
        out = {}
        for op, operand in want.items():
            value, ok = _fill(operand, row)
            if not ok:
                return (None, False)
            out[op] = value
        return (out, True)
    return (want, True)


def _param_values(want: object, params: dict) -> list[object]:
    """Every concrete value a param/constant predicate value can take."""
    if isinstance(want, str) and want.startswith("$"):
        return [params[want[1:]]]
    if isinstance(want, dict):
        out: list[object] = []
        for op, operand in want.items():
            for value in _param_values(operand, params):
                out.append({op: value})
        return out
    return [want]


def _combos(where: dict, params: dict, bindings: dict[str, list[dict]]
            ) -> list[tuple[dict, list[dict]]]:
    """Resolve a `where` into concrete predicates: one per joint parent binding.

    Fields binding the *same* parent block are filled from the *same* row, because
    they are fields of one parent row, not independent sets: crossing them would
    invent fan-out the request does not have.
    """
    bound: dict[str, list[str]] = {}
    for field_name, want in where.items():
        block = _ref_block(want)
        if block is not None:
            bound.setdefault(block, []).append(field_name)
    constants: list[dict] = [{}]
    for field_name, want in where.items():
        if _ref_block(want) is not None:
            continue
        constants = [{**predicate, field_name: value}
                     for predicate in constants
                     for value in _param_values(want, params)]
    parents: list[dict[str, dict]] = [{}]
    for block in bound:
        parents = [{**chosen, block: row} for chosen in parents for row in bindings[block]]
    combos: list[tuple[dict, list[dict]]] = []
    for chosen in parents:
        predicate: dict = {}
        ok = True
        for block, field_names in bound.items():
            for field_name in field_names:
                value, found = _fill(where[field_name], chosen[block])
                if not found:
                    ok = False
                    break
                predicate[field_name] = value
            if not ok:
                break
        if not ok:
            continue
        sources = list(chosen.values())
        for constant in constants:
            combos.append(({**constant, **predicate}, sources))
    return combos


def _dependency_order(blocks: dict, request_id: str) -> list[str]:
    """Block names in `needs` order (the DAG is acyclic by construction)."""
    order: list[str] = []
    while len(order) < len(blocks):
        ready = [name for name in blocks
                 if name not in order
                 and all(dep in order for dep in blocks[name].get("needs", []))]
        assert ready, f"cycle or dangling `needs` in request {request_id!r}"
        order.extend(ready)
    return order


def interpret(request: dict, world: JoinWorld) -> list[Hop]:
    """Resolve every read block of `request` against `world`, parents first."""
    params = request["params"]
    blocks = request["blocks"]
    bindings: dict[str, list[dict]] = {}
    depths: dict[str, int] = {}
    hops: list[Hop] = []
    for name in _dependency_order(blocks, request["id"]):
        block = blocks[name]
        depth = 1 + max((depths[dep] for dep in block.get("needs", [])
                         if dep in depths), default=0)
        for shape in READ_SHAPES:
            if shape not in block:
                continue
            entity, query = next(iter(block[shape].items()))
            where = query.get("where", {})
            limit = query.get("limit")
            groups = []
            for predicate, sources in _combos(where, params, bindings):
                rows = world.select(entity, predicate)
                if shape == "get":
                    rows = rows[:1]
                elif limit is not None:
                    rows = rows[:limit]
                groups.append(Group(predicate, rows, sources))
            bindings[name] = [row for group in groups for row in group.rows]
            depths[name] = depth
            hops.append(Hop(name, entity, depth, _referenced(where), limit, groups))
    return hops


def _parent_key_values(group: Group, key: str) -> set:
    return {source[key] for source in group.sources if key in source}


def _colocation(hops: list[Hop], key: str) -> tuple[int, int, int]:
    """(co-located parent groups, parent groups that matched, parent groups absent)."""
    co_located = matched = absent = 0
    for hop in hops:
        if not hop.dependent:
            continue
        for group in hop.groups:
            keys = _parent_key_values(group, key)
            if not group.rows:
                absent += 1
                continue
            matched += 1
            if keys and all(row.get(key) in keys for row in group.rows):
                co_located += 1
    return co_located, matched, absent


def _regions(hops: list[Hop], key: str) -> int:
    values = set()
    for hop in hops:
        for group in hop.groups:
            values |= _parent_key_values(group, key)
            for row in group.rows:
                if key in row:
                    values.add(row[key])
    return len(values)


def describe(request: dict, world: JoinWorld, shape: str) -> dict:
    """Per-request measurements: the four descriptors plus their raw counters."""
    hops = interpret(request, world)
    key = SHAPE_KEYS[shape]
    fanouts = [n for hop in hops if hop.dependent for n in hop.fanout()]
    co_located, matched, absent = _colocation(hops, key)
    candidates = {}
    for candidate in CANDIDATE_KEYS:
        hit, seen, _ = _colocation(hops, candidate)
        candidates[candidate] = (hit, seen)
    return {
        "fanout_width": max(fanouts) if fanouts else 0,
        "multihop_depth": max(hop.depth for hop in hops),
        "dependent_reads": sum(1 for hop in hops if hop.dependent),
        "read_hops": len(hops),
        "co_located": co_located,
        "colocated_parents": matched,
        "absent_parents": absent,
        "colocation_candidates": candidates,
        "regions": _regions(hops, key),
        "root_rows": max((len(hop.matched) for hop in hops if not hop.dependent), default=0),
        "hops": hops,
    }


def _share(numerator: int, denominator: int) -> float:
    return numerator / denominator if denominator else 0.0


def _hop_rows(hops: list[Hop], key: str) -> list[dict]:
    """Aggregate the hops by block name over the whole corpus (one row per block)."""
    order: list[str] = []
    for hop in hops:
        if hop.block not in order:
            order.append(hop.block)
    rows = []
    for block in order:
        own = [hop for hop in hops if hop.block == block]
        fanouts = [n for hop in own for n in hop.fanout()]
        co_located = matched = absent = 0
        for hop in own:
            hit, seen, missing = _colocation([hop], key)
            co_located, matched, absent = co_located + hit, matched + seen, absent + missing
        rows.append({
            "block": block,
            "entity": own[0].entity,
            "depth": own[0].depth,
            "parents": list(own[0].parents),
            "limit": own[0].limit,
            "fanout_median": median(fanouts) if fanouts else 0,
            "fanout_max": max(fanouts) if fanouts else 0,
            "rows_total": sum(len(hop.matched) for hop in own),
            "requests_with_no_match": sum(1 for hop in own if not hop.matched),
            "absent_parent_share": round(_share(absent, absent + matched), 4),
            "dependent": own[0].dependent,
            "key_colocated_share": round(_share(co_located, matched), 4),
        })
    return rows


def measure(seed: int, per_shape: int) -> dict:
    """Measure every shape over the deterministic corpus; returns the report dict."""
    gen = Generator(seed, 4 * per_shape)
    pairs = list(zip([n for _ in range(per_shape) for n in JOIN_NAMES],
                     gen.join_requests(per_shape)))
    buf = io.StringIO()
    for _name, request in pairs:
        buf.write(next(render([request])) + "\n")
    corpus = buf.getvalue().encode("utf-8")
    world = gen.world

    shapes: dict[str, dict] = {}
    for name in JOIN_NAMES:
        own = [request for request_name, request in pairs if request_name == name]
        described = [describe(request, world, name) for request in own]
        key = SHAPE_KEYS[name]
        fanout_max = [d["fanout_width"] for d in described]
        co_located = sum(d["co_located"] for d in described)
        matched = sum(d["colocated_parents"] for d in described)
        absent = sum(d["absent_parents"] for d in described)
        best_key, best_ratio = None, 0.0
        for candidate in CANDIDATE_KEYS:
            hit = sum(d["colocation_candidates"][candidate][0] for d in described)
            seen = sum(d["colocation_candidates"][candidate][1] for d in described)
            ratio = _share(hit, seen)
            if ratio > best_ratio:
                best_key, best_ratio = candidate, ratio
        hops = [hop for d in described for hop in d["hops"]]
        shapes[name] = {
            "colocation_key": key,
            "fanout_width": {
                "value": {"median": median(fanout_max), "max": max(fanout_max)},
                "method": (
                    "per dependent hop, the number of child rows one parent row reaches "
                    "after the block's declared `limit`, selected against "
                    "generator.JoinWorld; reported as the median and max over requests of "
                    "each request's widest dependent hop, over "
                    f"{len(own)} requests (seed {seed}). A root read's row count is not a "
                    "fan-out and is reported as root_rows. The model, not the engine, is "
                    "the population: engine fan-out additionally depends on a real history."
                ),
            },
            "multihop_depth": {
                "value": max(d["multihop_depth"] for d in described),
                "method": (
                    "number of reads on the longest `needs` path of the emitted request "
                    "(root read counted); counted from the block graph, constant per shape."
                ),
            },
            "dependent_read_share": {
                "value": round(_share(sum(d["dependent_reads"] for d in described),
                                      sum(d["read_hops"] for d in described)), 4),
                "method": (
                    "dependent read hops / all read hops, where a hop is dependent iff its "
                    "predicate binds `$block.field` from an earlier block's rows and so "
                    f"cannot be issued before that block returns; counted over {len(own)} "
                    "requests."
                ),
            },
            "colocation_potential": {
                "value": round(_share(co_located, matched), 4),
                "method": (
                    f"share of dependent parent rows whose every matched child row carries "
                    f"the parent's `{key}` value, over parent rows that matched at least one "
                    "row; such a child would share a region with its parent under a region "
                    "key materialised on that field. Parent rows with no match are excluded "
                    "and reported as absent_parent_share; a hop may pass on a tiny dimension "
                    "(relative cost is not implied by this ratio alone)."
                ),
            },
            "absent_parent_share": {
                "value": round(_share(absent, absent + matched), 4),
                "method": (
                    "dependent parent rows whose hop matched no row / all dependent parent "
                    "rows; this is the anti-join's answer and it has no row to place, so no "
                    "co-location key can make it cheap."
                ),
            },
            "colocation_best_single_key": {
                "value": {"key": best_key, "ratio": round(best_ratio, 4)},
                "method": (
                    "colocation_potential maximised over the stated candidate single keys "
                    f"{list(CANDIDATE_KEYS)}; below 1.0 means no single key co-locates every "
                    "hop of the shape, so per-hop placement (L2) or per-region parameters "
                    "decide the remainder."
                ),
            },
            "regions_per_request": {
                "value": round(_share(sum(d["regions"] for d in described), len(described)), 2),
                "method": (
                    f"distinct `{key}` values carried by the request's parent and child rows, "
                    "averaged over the corpus: the number of regions a region keyed on that "
                    "field makes the request touch (a per-region-parameter cost driver, not a "
                    "co-location one)."
                ),
            },
            "root_rows": {
                "value": {"median": median(d["root_rows"] for d in described),
                          "max": max(d["root_rows"] for d in described)},
                "method": (
                    "rows the request's root read returns (the independent hop), median and "
                    "max over the corpus; it bounds how many parent rows the dependent hops "
                    "fan out from."
                ),
            },
            "hops": _hop_rows(hops, key),
        }
    return {
        "schema": SCHEMA,
        "corpus": {
            "seed": seed,
            "per_shape": per_shape,
            "requests": len(pairs),
            "sha256": hashlib.sha256(corpus).hexdigest(),
        },
        "model": {"source": "flashdb_harness.generator.JoinWorld",
                  "notes": MODEL_NOTES},
        "shapes": shapes,
    }


def print_table(report: dict, out=sys.stdout) -> None:
    """The descriptor table as committed Markdown."""
    print("| shape | co-location key | fan-out width (median / max) | multi-hop depth "
          "| dependent-read share | co-location potential | best single key | absent share "
          "| regions / request |", file=out)
    print("|---|---|---|---|---|---|---|---|---|", file=out)
    for name, shape in report["shapes"].items():
        fan = shape["fanout_width"]["value"]
        best = shape["colocation_best_single_key"]["value"]
        print(f"| `{name}` | `{shape['colocation_key']}` | "
              f"{fan['median']} / {fan['max']} | {shape['multihop_depth']['value']} | "
              f"{shape['dependent_read_share']['value']} | "
              f"{shape['colocation_potential']['value']} | "
              f"`{best['key']}` {best['ratio']} | "
              f"{shape['absent_parent_share']['value']} | "
              f"{shape['regions_per_request']['value']} |", file=out)
    print(file=out)
    print("| shape | hop | entity | depth | parents | limit | fan-out median / max "
          "| rows total | no-match requests | absent share | key co-located share |",
          file=out)
    print("|---|---|---|---|---|---|---|---|---|---|---|", file=out)
    for name, shape in report["shapes"].items():
        for hop in shape["hops"]:
            print(f"| `{name}` | `{hop['block']}` | {hop['entity']} | {hop['depth']} | "
                  f"{', '.join(hop['parents']) or '-'} | {hop['limit'] or '-'} | "
                  f"{hop['fanout_median']} / {hop['fanout_max']} | {hop['rows_total']} | "
                  f"{hop['requests_with_no_match']} | "
                  f"{hop['absent_parent_share']} | "
                  f"{'-' if not hop['dependent'] else hop['key_colocated_share']} |", file=out)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="measure the join-heavy shapes' descriptors")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--per-shape", type=int, default=25)
    ap.add_argument("--out", type=Path, default=None, help="write the JSON report here")
    a = ap.parse_args(argv)
    report = measure(a.seed, a.per_shape)
    print_table(report)
    print(f"corpus: {report['corpus']['requests']} requests, "
          f"sha256 {report['corpus']['sha256']}")
    if a.out is not None:
        a.out.write_text(json.dumps(report, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
