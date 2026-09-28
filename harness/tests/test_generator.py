"""生成器的确定性、偏斜与结构测试（无外部依赖）。"""

import hashlib
import io
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness.generator import Generator, Zipf, Rng, render


def render_bytes(seed: int, n: int = 500) -> bytes:
    g = Generator(seed, n)
    buf = io.StringIO()
    for line in render(g.requests()):
        buf.write(line + "\n")
    return buf.getvalue().encode()


def test_deterministic_same_seed():
    assert render_bytes(7) == render_bytes(7)


def test_different_seed_differs():
    assert render_bytes(7) != render_bytes(8)


def test_zipf_skew_is_real():
    rng = Rng(42)
    z = Zipf(rng, 10_000, 1.0)
    draws = [z.draw() for _ in range(50_000)]
    top1 = sum(1 for d in draws if d <= 100) / len(draws)
    assert top1 >= 0.30, f"top-1% of skus covered only {top1:.0%} — Zipf 偏斜未生效"
    assert len(set(draws)) > 1_000, "访问几乎集中在一个键上，Zipf 参数错误"


def test_requests_are_structurally_valid():
    g = Generator(7, 200)
    for r in g.requests():
        assert r["id"] and r["class"] and isinstance(r["blocks"], dict)
        for name, block in r["blocks"].items():
            assert name, "块名不得为空"
            shapes = sum(k in block for k in ("get", "find", "put", "patch", "del", "ops"))
            assert shapes == 1, f"块 {name} 必须恰好一个形状"
            for dep in block.get("needs", []):
                assert dep in r["blocks"], f"needs 引用不存在的块 {dep}"


def test_order_flow_fixture_matches_generator():
    fixture = (Path(__file__).parent / "fixtures" / "order_flow_seed42.jsonl").read_text()
    assert fixture == next(render([Generator(42, 1).order_flow()])) + "\n"


def test_mixed_replay_corpus_is_frozen():
    fixture = (Path(__file__).parent / "fixtures" / "mixed_seed42_20.jsonl").read_text()
    assert fixture == "".join(line + "\n" for line in render(Generator(42, 20).requests()))
    assert hashlib.sha256(fixture.encode()).hexdigest() == "929822594483301f5951e98746c60932d7bfd82408780555a231e9d0a28c5264"


def test_order_flow_exercises_motivating_case():
    g = Generator(1, 50)
    flows = [r for r in g.requests() if "take" in r["blocks"]]
    assert flows, "order_flow 必须出现"
    take = flows[0]["blocks"]["take"]
    probe = take["ops"][0]["patch"]["Stock"]["where"]
    assert probe["on_hand"] == {"$gte": "$qty"}, "probe 必须内嵌守卫"
    assert len(take["ops"]) >= 3, "多写原子块必须覆盖 跨实体"
    assert "else" in take, "缺货回退必须存在"


def test_mix_contains_all_kinds():
    g = Generator(3, 2_000)
    kinds = set()
    for r in g.requests():
        if "take" in r["blocks"]:
            kinds.add("order_flow")
        else:
            kinds.add("query")
    assert kinds == {"order_flow", "query"}
