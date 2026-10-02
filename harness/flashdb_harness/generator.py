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

# The single spelling of each dimension key. `seed.py` imports these so the
# seeding trace and the flow's Zipfian lookups can never disagree on the names.
def email_name(i: int) -> str:
    return f"c{i:06d}@example.com"


def sku_name(rank: int) -> str:
    """Name of Zipf rank `rank` (1-based); rank 1 is the hottest sku."""
    return f"S{rank:06d}"


def loc_name(j: int) -> str:
    return f"L{j:03d}"

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


def pick_kind(rng: "Rng", mix: dict[str, int], cum: list[int], total: int) -> str:
    """Draw a request-shape name from integer weights (the single selection rule).

    `Generator.requests` and the load-profile generator both use this so the shape
    selection can never disagree. Integers only: no float accumulation.
    """
    u = rng.next_u64() % total
    return next(k for k, c in zip(mix, cum) if u < c)


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
        self.emails = [email_name(i) for i in range(n_customers)]
        self.customer_no = [f"C{i:06d}" for i in range(n_customers)]
        self.locs = [loc_name(i) for i in range(n_loc)]
        self.req_no = 0
        self.order_no = 0
        # Optional deterministic "flash sale": a fraction of sku draws in the
        # selected shape are replaced by one hot sku. Inactive by default, so the
        # rng stream and the frozen corpora are unchanged unless a profile sets it.
        self.spike_sku: str | None = None
        self.spike_num = 0
        self.spike_denom = 100

    def set_key_spike(self, sku_rank: int | None, spike_pct: int = 0) -> None:
        """Spike `sku_rank` (1-based) in `spike_pct`% of sku draws, or clear it."""
        if sku_rank is None or spike_pct <= 0:
            self.spike_sku = None
            self.spike_num = 0
            return
        assert sku_rank >= 1, "sku rank is 1-based"
        assert 0 < spike_pct <= 100
        self.spike_sku = sku_name(sku_rank)
        self.spike_num = spike_pct

    def _next_req_id(self) -> str:
        self.req_no += 1
        return f"r-{self.req_no:08d}"

    def _sku(self) -> str:
        rank = self.zipf.draw()
        if self.spike_sku is not None and self.rng.below(self.spike_denom) < self.spike_num:
            return self.spike_sku
        return sku_name(rank)

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

    def shape(self, kind: str) -> dict:
        """Build one request of the named shape; shape name == method name."""
        return getattr(self, kind)()

    def requests(self) -> Iterator[dict]:
        for _ in range(self.n_requests):
            yield self.shape(pick_kind(self.rng, MIX, MIX_CUM, MIX_TOTAL))


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
    a = ap.parse_args(argv)
    g = Generator(a.seed, a.requests, a.customers, a.skus, a.locs, a.theta)
    out = sys.stdout if a.out == "-" else open(a.out, "w", encoding="utf-8", newline="\n")
    for line in render(g.requests()):
        out.write(line + "\n")
    if out is not sys.stdout:
        out.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
