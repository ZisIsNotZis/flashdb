"""确定性 workload 生成器：inventory 场景（订单流 + 查询流）。

确定性保证：随机源只用自实现的 SplitMix64 与整数 Zipf CDF——
不用 `random`、不用浮点累积，因此同一 seed 在任何 Python 版本、
任何平台上产出**字节级相同**的 trace（JSONL，键排序）。

请求形状遵循 `docs/contracts.md`：命名块 DAG、按块原子、
probe 即守卫（`where` 内嵌 `$gte`）、多写原子块、`else` 回退。
"""

from __future__ import annotations

import argparse
import itertools
import json
import sys
from typing import Iterator

MASK = (1 << 64) - 1


class Rng:
    """SplitMix64：自实现，字节级确定，跨平台跨版本稳定。"""

    def __init__(self, seed: int) -> None:
        self.s = seed & MASK

    def next_u64(self) -> int:
        self.s = (self.s + 0x9E3779B97F4A7C15) & MASK
        z = self.s
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
        return z ^ (z >> 31)

    def below(self, n: int) -> int:
        return self.next_u64() % n

    def pick(self, xs: list[str]) -> str:
        return xs[self.below(len(xs))]


class Zipf:
    """整数 Zipf CDF（无浮点）：draw() 返回 1..n 的 rank，rank 越小越热。"""

    def __init__(self, rng: Rng, n: int, theta: float = 1.0, precision: int = 2**32) -> None:
        assert n >= 1
        w = [max(1, int(precision / (i**theta))) for i in range(1, n + 1)]
        self.cum = list(itertools.accumulate(w))
        self.total = self.cum[-1]
        self.rng = rng

    def draw(self) -> int:
        u = self.rng.next_u64() % self.total
        lo, hi = 0, len(self.cum) - 1
        while lo < hi:
            mid = (lo + hi) // 2
            if self.cum[mid] <= u:
                lo = mid + 1
            else:
                hi = mid
        return lo + 1


MIX = {"order_flow": 70, "stock_lookup": 15, "customer_lookup": 10, "movements_scan": 5}
MIX_CUM = list(itertools.accumulate(MIX[k] for k in MIX))
MIX_TOTAL = MIX_CUM[-1]
MIX_NAMES = list(MIX)


class Generator:
    def __init__(
        self,
        seed: int,
        n_requests: int,
        n_customers: int = 5_000,
        n_sku: int = 10_000,
        n_loc: int = 8,
        theta: float = 1.0,
    ) -> None:
        assert n_requests >= 1
        self.rng = Rng(seed)
        self.zipf = Zipf(self.rng, n_sku, theta)
        self.n_requests = n_requests
        self.n_customers = n_customers
        self.n_loc = n_loc
        self.emails = [f"c{i:06d}@example.com" for i in range(n_customers)]
        self.customer_no = [f"C{i:06d}" for i in range(n_customers)]
        self.locs = [f"L{i:03d}" for i in range(n_loc)]
        self.req_no = 0
        self.order_no = 0

    def _next_req_id(self) -> str:
        self.req_no += 1
        return f"r-{self.req_no:08d}"

    def _sku(self) -> str:
        return f"S{self.zipf.draw():06d}"

    def order_flow(self) -> dict:
        self.order_no += 1
        sku, loc = self._sku(), self.rng.pick(self.locs)
        qty = 1 + self.rng.below(5)
        email = self.rng.pick(self.emails)
        order_no = f"O{self.order_no:08d}"
        return {
            "id": self._next_req_id(),
            "class": {"durability": "batched", "retry_horizon_s": 300},
            "params": {"sku": sku, "loc": loc, "qty": qty, "email": email, "order_no": order_no},
            "blocks": {
                "cust": {"find": {"Customer": {"email": "$email"}}},
                "take": {
                    "needs": ["cust"],
                    "ops": [
                        {
                            "patch": {
                                "Stock": {
                                    "where": {
                                        "sku": "$sku",
                                        "loc": "$loc",
                                        "on_hand": {"$gte": "$qty"},
                                    },
                                    "set": {"$inc": {"on_hand": "-$qty"}},
                                }
                            }
                        },
                        {
                            "put": {
                                "StockMovement": {
                                    "movement_no": f"M{self.order_no:08d}",
                                    "sku": "$sku",
                                    "loc": "$loc",
                                    "delta": "-$qty",
                                    "order_no": "$order_no",
                                    "stock": {"sku": "$sku", "loc": "$loc"},
                                }
                            }
                        },
                        {
                            "put": {
                                "Order": {
                                    "order_no": "$order_no",
                                    "buyer": "$cust._ref",
                                    "status": "open",
                                    "lines": [{"sku": "$sku", "qty": "$qty"}],
                                }
                            }
                        },
                    ],
                    "else": [
                        {
                            "put": {
                                "Backorder": {
                                    "sku": "$sku", "qty": "$qty", "order_no": "$order_no"
                                }
                            }
                        }
                    ],
                },
            },
        }

    def stock_lookup(self) -> dict:
        sku, loc = self._sku(), self.rng.pick(self.locs)
        return {
            "id": self._next_req_id(),
            "class": {"durability": "batched", "max_staleness": 2},
            "blocks": {"q": {"get": {"Stock": {"where": {"sku": sku, "loc": loc}}}}},
        }

    def customer_lookup(self) -> dict:
        return {
            "id": self._next_req_id(),
            "class": {"durability": "batched"},
            "blocks": {
                "q": {"find": {"Customer": {"email": self.rng.pick(self.emails)}}}
            },
        }

    def movements_scan(self) -> dict:
        order_no = f"O{1 + self.rng.below(max(1, self.order_no)):08d}"
        return {
            "id": self._next_req_id(),
            "class": {"durability": "batched", "max_staleness": 5},
            "blocks": {
                "q": {"find": {"StockMovement": {"where": {"order_no": order_no}},
                                "order": ["movement_no"], "limit": 100}}
            },
        }

    def requests(self) -> Iterator[dict]:
        for _ in range(self.n_requests):
            u = self.rng.next_u64() % MIX_TOTAL
            kind = MIX_NAMES[next(i for i, c in enumerate(MIX_CUM) if u < c)]
            yield {
                "order_flow": self.order_flow,
                "stock_lookup": self.stock_lookup,
                "customer_lookup": self.customer_lookup,
                "movements_scan": self.movements_scan,
            }[kind]()


def render(reqs: Iterator[dict]) -> Iterator[str]:
    for r in reqs:
        yield json.dumps(r, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--requests", type=int, default=10_000)
    ap.add_argument("--customers", type=int, default=5_000)
    ap.add_argument("--skus", type=int, default=10_000)
    ap.add_argument("--locs", type=int, default=8)
    ap.add_argument("--theta", type=float, default=1.0)
    ap.add_argument("--out", default="-")
    a = ap.parse_args()
    g = Generator(a.seed, a.requests, a.customers, a.skus, a.locs, a.theta)
    out = sys.stdout if a.out == "-" else open(a.out, "w", encoding="utf-8")
    for line in render(g.requests()):
        out.write(line + "\n")
    if out is not sys.stdout:
        out.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
