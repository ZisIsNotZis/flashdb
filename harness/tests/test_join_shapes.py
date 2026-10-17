"""The join-heavy shapes (M2 / ticket 81): determinism, dialect, and descriptors.

The flat flow's own guarantees live in `test_generator.py`; this file covers the
four shapes ticket 81 added and re-asserts that their arrival left the frozen
`MIX` corpora byte-identical, because `Generator.requests()` is the dataset every
earlier measurement was taken on.
"""

import hashlib
import io
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness.generator import JOIN_NAMES, MIX, Generator, render
from flashdb_harness.join_descriptors import (
    SHAPE_KEYS, describe, interpret, measure, read_groups)

FIXTURES = Path(__file__).parent / "fixtures"
DESCRIPTORS_JSON = (Path(__file__).resolve().parents[2]
                    / ".scratch" / "81-join-heavy-workloads" / "descriptors.json")
JOIN_CORPUS_SHA256 = "ecc271e43efcc4af0184dccec102920d61f5c4181e0c1351e30db59dd31a8d6a"
MIX_CORPUS_SHA256 = "929822594483301f5951e98746c60932d7bfd82408780555a231e9d0a28c5264"
HOT_ORDERS_SHA256 = "d803afa5bd8cd459abbadb9e9bd707b491c96c2ccdedf372b7a94930e8758855"

# The values the committed report tables quote (seed 42, 25 requests per shape). A
# change to the counting rules must move these tests, so neither `descriptors.json`
# nor a report table can go stale silently.
PINNED_SHAPES = {
    "star_fanout": {"key": "sku", "fanout": (2, 5), "depth": 3,
                    "dependent_share": 0.75, "colocation": 0.6479,
                    "best": ("sku", 0.6479), "absent": 0.0533, "regions": 1.0},
    "multi_hop": {"key": "order_no", "fanout": (614, 614), "depth": 5,
                  "dependent_share": 0.8333, "colocation": 0.5487,
                  "best": ("order_no", 0.5487), "absent": 0.2539, "regions": 410.6},
    "as_of": {"key": "sku", "fanout": (1, 1), "depth": 2,
              "dependent_share": 0.5, "colocation": 1.0,
              "best": ("sku", 1.0), "absent": 0.0, "regions": 1.0},
    "anti_join": {"key": "sku", "fanout": (1, 1), "depth": 2,
                  "dependent_share": 0.5, "colocation": 1.0,
                  "best": ("sku", 1.0), "absent": 0.1245, "regions": 106.64},
}

# (read groups, dependent read groups, child rows, root rows) per request, report §2.3.
PINNED_READ_GROUPS = {
    "star_fanout": (4.0, 3.0, 4.16, 1.0),
    "multi_hop": (1216.32, 1215.32, 1582.56, 4.44),
    "as_of": (2.88, 1.88, 1.88, 1.88),
    "anti_join": (107.64, 106.64, 132.44, 151.28),
}


def join_bytes(seed: int = 42, per_shape: int = 25) -> bytes:
    g = Generator(seed, 4 * per_shape)
    buf = io.StringIO()
    for line in render(g.join_requests(per_shape)):
        buf.write(line + "\n")
    return buf.getvalue().encode()


def test_join_corpus_is_frozen():
    """The measured corpus is a dataset: same seed, same bytes, committed hash."""
    fixture = (FIXTURES / "join_shapes_seed42.jsonl").read_bytes()
    assert fixture == join_bytes()
    assert hashlib.sha256(fixture).hexdigest() == JOIN_CORPUS_SHA256
    assert fixture.count(b"\n") == 100


def test_every_join_shape_is_deterministic():
    """Each shape alone: a fresh generator replays it byte for byte."""
    for name in JOIN_NAMES:
        left, right = Generator(7, 4), Generator(7, 4)
        first = [next(render([left.shape(name)])) for _ in range(20)]
        second = [next(render([right.shape(name)])) for _ in range(20)]
        assert first == second, f"{name} 不是确定性的"


def test_join_corpus_is_independent_of_the_flat_mix():
    """Different seeds give different join corpora (the stream really is seed-driven)."""
    assert join_bytes(seed=42) != join_bytes(seed=43)


def test_flat_mix_corpus_hashes_are_unchanged():
    """Adding the join shapes must not move the dataset earlier slices measured on."""
    assert MIX == {"order_flow": 70, "stock_lookup": 15, "customer_lookup": 10,
                   "movements_scan": 5}
    mixed = (FIXTURES / "mixed_seed42_20.jsonl").read_bytes()
    assert hashlib.sha256(mixed).hexdigest() == MIX_CORPUS_SHA256
    assert mixed == "".join(line + "\n" for line in render(Generator(42, 20).requests())).encode()
    hot = (FIXTURES / "hot_orders_seed42.jsonl").read_bytes()
    assert hashlib.sha256(hot).hexdigest() == HOT_ORDERS_SHA256


def test_join_shapes_stay_in_the_request_dialect():
    """One shape per block, `needs` resolves, and every `$block.field` names a block."""
    g = Generator(42, 4)
    for request in g.join_requests(5):
        assert request["id"] and request["class"] and isinstance(request["params"], dict)
        blocks = request["blocks"]
        for name, block in blocks.items():
            shapes = [k for k in ("get", "find", "put", "patch", "del", "ops") if k in block]
            assert len(shapes) == 1, f"块 {name} 必须恰好一个形状"
            for dep in block.get("needs", []):
                assert dep in blocks, f"needs 引用不存在的块 {dep}"
            for ref in _refs(block[shapes[0]]):
                assert ref in blocks, f"{name} 绑定了不存在的块 {ref}"
                assert blocks[ref].keys() & {"get", "find"}, f"{name} 必须绑定读块 {ref}"


def _refs(query: object) -> list[str]:
    """Block names a query binds through a `$block.field` path."""
    found: list[str] = []

    def walk(value: object) -> None:
        if isinstance(value, str) and value.startswith("$") and "." in value:
            found.append(value[1:].split(".", 1)[0])
        elif isinstance(value, dict):
            for item in value.values():
                walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    walk(query)
    return found


def test_every_join_shape_reaches_modelled_rows():
    """A shape whose root read matches nothing would measure an empty request."""
    g = Generator(42, 1)
    for name in JOIN_NAMES:
        for _ in range(10):
            request = g.shape(name)
            hops = interpret(request, g.world)
            root = next(hop for hop in hops if not hop.dependent)
            assert root.matched, f"{name} 的根读没有任何行"


def test_descriptors_are_reproducible_and_named():
    """Every descriptor is measured from the corpus and identical on a rerun."""
    first, second = measure(42, 5), measure(42, 5)
    assert first == second
    assert first["corpus"]["requests"] == 20
    for name in JOIN_NAMES:
        shape = first["shapes"][name]
        for field in ("fanout_width", "multihop_depth", "dependent_read_share",
                      "colocation_potential", "absent_parent_share",
                      "colocation_best_single_key", "regions_per_request", "root_rows"):
            assert shape[field]["method"], f"{name}.{field} 缺方法说明"
        assert shape["multihop_depth"]["value"] >= 2
        assert 0.0 < shape["dependent_read_share"]["value"] <= 1.0
        assert 0.0 <= shape["colocation_potential"]["value"] <= 1.0
        assert shape["colocation_key"] == SHAPE_KEYS[name]
        # the best single key is a maximum over candidates that include the shape's key
        assert shape["colocation_best_single_key"]["value"]["ratio"] >= \
            shape["colocation_potential"]["value"]


def test_committed_descriptor_values_are_pinned():
    """The report tables read these: a stale JSON or a stale report must fail here."""
    report = measure(42, 25)
    assert report["corpus"] == {"seed": 42, "per_shape": 25, "requests": 100,
                                "sha256": JOIN_CORPUS_SHA256}
    assert json.loads(DESCRIPTORS_JSON.read_text()) == report
    for name, want in PINNED_SHAPES.items():
        shape = report["shapes"][name]
        fan = shape["fanout_width"]["value"]
        assert shape["colocation_key"] == want["key"], name
        assert (fan["median"], fan["max"]) == want["fanout"], name
        assert shape["multihop_depth"]["value"] == want["depth"], name
        assert shape["dependent_read_share"]["value"] == want["dependent_share"], name
        assert shape["colocation_potential"]["value"] == want["colocation"], name
        best = shape["colocation_best_single_key"]["value"]
        assert (best["key"], best["ratio"]) == want["best"], name
        assert shape["absent_parent_share"]["value"] == want["absent"], name
        assert shape["regions_per_request"]["value"] == want["regions"], name
    # one value, one spelling: an integral median is emitted as an int, not `1.0`
    supply = {hop["block"]: hop for hop in report["shapes"]["anti_join"]["hops"]}["supply"]
    assert type(supply["fanout_median"]) is int and supply["fanout_median"] == 1


def test_read_group_cost_model_is_committed():
    """§2.3's read groups and child rows come from this module, not a shell heredoc."""
    groups = read_groups(42, 25)
    for name, want in PINNED_READ_GROUPS.items():
        row = groups[name]
        assert (row["read_groups_per_request"],
                row["dependent_read_groups_per_request"],
                row["child_rows_per_request"],
                row["root_rows_per_request"]) == want, name
        # the only non-dependent group is the root read
        assert row["read_groups_per_request"] - row["dependent_read_groups_per_request"] == 1.0
    assert groups["multi_hop"]["read_groups_by_hop"] == {
        "items": 1.0, "products": 4.44, "lines": 4.44,
        "orders": 410.6, "returns": 410.6, "buyers": 385.24}


def test_multi_hop_fanout_width_is_the_hot_sku():
    """614 is the hottest sku's line count, drawn into 13 of the 25 requests: bimodal, no median."""
    g = Generator(42, 100)
    requests = list(g.join_requests(25))[1::4]        # JOIN_NAMES order: star, multi_hop, ...
    widths = [describe(request, g.world, "multi_hop")["fanout_width"]
              for request in requests]
    assert widths.count(614) == 13
    assert max(width for width in widths if width != 614) == 311
    assert sorted(widths)[12] == 614                  # the median sits on the hot sku


def test_descriptors_read_the_emitted_request_not_a_guess():
    """Fan-out and depth are counted from the request: a wider limit widens fan-out."""
    g = Generator(42, 1)
    request = g.star_fanout()
    hop = {h.block: h for h in interpret(request, g.world)}["movements"]
    assert hop.entity == "StockMovement" and hop.limit == 128
    assert max(hop.fanout()) <= 128
    capped = dict(request)
    capped["blocks"] = {**request["blocks"],
                        "movements": {"needs": ["line"], "find": {"StockMovement": {
                            **request["blocks"]["movements"]["find"]["StockMovement"],
                            "limit": 1}}}}
    capped_hop = {h.block: h for h in interpret(capped, g.world)}["movements"]
    assert max(capped_hop.fanout()) <= 1          # the declared bound is what is measured
    described = describe(request, g.world, "star_fanout")
    assert described["multihop_depth"] == 3        # line -> product -> supplier
