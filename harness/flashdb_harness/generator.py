"""确定性 workload 生成器：inventory 场景（订单流 + 查询流 + join 重形状）。

确定性保证：随机源只用自实现的 SplitMix64 与整数 Zipf CDF——
不用 `random`、不用浮点累积，因此同一 seed 在任何 Python 版本、
任何平台上产出**字节级相同**的 trace（JSONL，键排序）。

请求形状遵循 `docs/contracts.md`：命名块 DAG、按块原子、
probe 即守卫（`where` 内嵌 `$gte`）、多写原子块、`else` 回退。

两组形状，互不影响：

- `MIX` —— 平铺 inventory 流（`order_flow` / `stock_lookup` / `customer_lookup` /
  `movements_scan`），即 `Generator.requests()`，其冻结语料与哈希不得改动；
- `JOIN_NAMES` —— ticket 81（M2）的四个 join 重形状（`star_fanout` / `multi_hop` /
  `as_of` / `anti_join`），由 `Generator.join_requests()` 单独发出，定义在
  `JoinWorld` 这个确定性数据模型之上，描述符由 `join_descriptors.py` 测量。
  它们不进 `MIX`，因为 MIX 的字节序列已被 `tests/fixtures/*.jsonl` 冻结。
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

# The join-heavy shapes (M2 / ticket 81) are emitted as a separate, equal-weight
# stream so the frozen `MIX` corpora and their hashes are untouched by their
# arrival. The names are the shape methods, in emission order.
JOIN_NAMES = ("star_fanout", "multi_hop", "as_of", "anti_join")


def pick_kind(rng: "Rng", mix: dict[str, int], cum: list[int], total: int) -> str:
    """Draw a request-shape name from integer weights (the single selection rule).

    `Generator.requests` and the load-profile generator both use this so the shape
    selection can never disagree. Integers only: no float accumulation.
    """
    u = rng.next_u64() % total
    return next(k for k, c in zip(mix, cum) if u < c)


# Comparison operators the join-heavy shapes' predicates use (all integer).
OPS = {
    "$gt": lambda a, b: a is not None and b is not None and a > b,
    "$gte": lambda a, b: a is not None and b is not None and a >= b,
    "$lt": lambda a, b: a is not None and b is not None and a < b,
    "$lte": lambda a, b: a is not None and b is not None and a <= b,
}


def matches(row: dict, where: dict) -> bool:
    """Does `row` satisfy the resolved predicate `where`?"""
    for field, want in where.items():
        have = row.get(field)
        if isinstance(want, dict):
            for op, operand in want.items():
                if not OPS[op](have, operand):
                    return False
        elif have != want:
            return False
    return True


class JoinWorld:
    """The data model the four join-heavy shapes are defined against (M2 / ticket 81).

    The flat inventory flow never walks from one of these entities to another, so
    the fan-out a join shape induces can only be counted against a modelled data
    set. This is that model: deterministic dimension and fact rows built from
    integer arithmetic plus the same `Rng`/`Zipf` skew the flow uses, so that
    `join_descriptors` can select against it and count the rows a request's
    predicates reach.

    Modelled assumptions, stated so a real data set can falsify them:

    - a sku with `(rank * 11) % 7 == 0` has no supply movement at all, so it is a
      lost sale whenever it is demanded (~1/7 of the catalogue);
    - movements are supply only (`delta > 0`); consumption is not modelled;
    - one product row per sku, one supplier row per supplier, `1 + rank % 3`
      SCD-2 versions per sku with a boundary every 10 days;
    - `orders_per_day` orders per day, 1-5 lines per order, 1-8 products per lot,
      0-2 returns per order.

    Nothing here is measured from the engine: it is the modelled population the
    emitted requests address, and the descriptor script reports it as such.
    """

    def __init__(
        self,
        seed: int = 1,
        n_sku: int = 10_000,
        n_suppliers: int = 64,
        n_customers: int = 5_000,
        n_locs: int = 8,
        n_orders: int = 2_000,
        n_lots: int = 500,
        n_days: int = 40,
        orders_per_day: int = 50,
        theta: float = 1.0,
    ) -> None:
        assert n_sku >= 1 and n_suppliers >= 1 and n_customers >= 1 and n_locs >= 1
        assert n_orders >= 1 and n_lots >= 1 and n_days >= 1 and orders_per_day >= 1
        rng = Rng(seed)
        zipf = Zipf(rng, n_sku, theta)
        self.n_orders = n_orders
        self.n_lots = n_lots
        self.n_days = n_days
        self.orders_per_day = orders_per_day

        self.tables: dict[str, list[dict]] = {}
        self._indices: dict[tuple[str, str], dict[object, list[dict]]] = {}
        self.tables["Supplier"] = [
            {"supplier_no": f"SUP{i:04d}", "lead_time_days": 2 + (i * 13) % 27,
             "region": f"R{i % 4}"}
            for i in range(n_suppliers)
        ]
        self.tables["Product"] = [
            {"sku": sku_name(rank),
             "supplier_no": f"SUP{rank % n_suppliers:04d}",
             "lead_time_days": 1 + (rank * 7) % 21,
             "category": f"K{rank % 32:02d}"}
            for rank in range(1, n_sku + 1)
        ]
        # SCD-2 dimension: one version row per (sku, validity interval).
        versions: list[dict] = []
        for rank in range(1, n_sku + 1):
            sku = sku_name(rank)
            n_versions = 1 + rank % 3
            for v in range(n_versions):
                valid_from = v * 10
                valid_to = (v + 1) * 10 if v + 1 < n_versions else n_days + 1
                versions.append({
                    "sku": sku, "valid_from": valid_from, "valid_to": valid_to,
                    "supplier_no": f"SUP{(rank + v) % n_suppliers:04d}",
                    "lead_time_days": 1 + (rank * 7 + v * 5) % 21,
                })
        self.tables["ProductVersion"] = versions
        self.tables["Customer"] = [
            {"email": email_name(i), "region": f"R{i % 4}"} for i in range(n_customers)
        ]

        orders: list[dict] = []
        lines: list[dict] = []
        lines_by_order: dict[str, list[dict]] = {}
        for o in range(1, n_orders + 1):
            order_no = f"O{o:08d}"
            day = 1 + (o - 1) // orders_per_day
            orders.append({
                "order_no": order_no,
                "buyer_email": email_name(rng.below(n_customers)),
                "day": day,
                "status": "closed" if o % 3 == 0 else "open",
            })
            own: list[dict] = []
            for j in range(1 + rng.below(5)):
                own.append({"order_no": order_no, "line_no": j + 1,
                            "sku": sku_name(zipf.draw()), "qty": 1 + rng.below(5), "day": day})
            lines.extend(own)
            lines_by_order[order_no] = own
        self.tables["Order"] = orders
        self.tables["OrderLine"] = lines
        self.lines_by_order = lines_by_order

        lots = [{"lot_no": f"LOT{i:05d}", "received_day": 1 + (i - 1) % n_days}
                for i in range(1, n_lots + 1)]
        lot_items: list[dict] = []
        for i in range(1, n_lots + 1):
            lot_no = f"LOT{i:05d}"
            # Supply follows demand: a lot's products are drawn from the same Zipfian
            # sku distribution as the order lines, otherwise a recall trace would run
            # into cold skus no order ever references and measure nothing.
            skus: list[str] = []
            for k in range(1 + rng.below(8)):
                sku = sku_name(zipf.draw())
                if not skus or sku not in skus:
                    skus.append(sku)
            for sku in skus:
                lot_items.append({"lot_no": lot_no, "sku": sku})
        self.tables["Lot"] = lots
        self.tables["LotItem"] = lot_items

        returns: list[dict] = []
        for order in orders:
            o = int(order["order_no"][1:])
            n_returns = (1 if o % 5 == 0 else 0) + (1 if o % 23 == 0 else 0)
            first_sku = lines_by_order[order["order_no"]][0]["sku"]
            lot_no = f"LOT{1 + (int(first_sku[1:]) % n_lots):05d}"
            for j in range(n_returns):
                returns.append({"return_no": f"RT{o:06d}-{j}", "order_no": order["order_no"],
                                "lot_no": lot_no, "day": order["day"]})
        self.tables["Return"] = returns

    def has_supply(self, sku: str) -> bool:
        """Does this sku ever receive stock inside the modelled window?"""
        return (int(sku[1:]) * 11) % 7 != 0

    def movements(self, sku: str, loc: str) -> list[dict]:
        """The synthesised supply history for one `(sku, loc)`; empty for a lost sku.

        A movement history is unbounded in a real deployment, so it is derived per
        key instead of materialised: `1 + (rank*37 + loc*11) % 5` supply rows with
        deterministic days and deltas.
        """
        if not self.has_supply(sku):
            return []
        rank, loc_index = int(sku[1:]), int(loc[1:])
        out = []
        for i in range(1 + (rank * 37 + loc_index * 11) % 5):
            out.append({
                "movement_no": f"M{rank * 100_000 + loc_index * 1_000 + i:012d}",
                "sku": sku, "loc": loc, "delta": 5 + (rank + i) % 20,
                "day": 1 + (rank * 7 + loc_index * 13 + i * 29) % self.n_days,
                "order_no": f"O{1 + (rank * 3 + i) % self.n_orders:08d}",
            })
        return out

    def _index(self, entity: str, field_name: str) -> dict[object, list[dict]]:
        """Equality index over one field of a materialised table (built once)."""
        key = (entity, field_name)
        if key not in self._indices:
            index: dict[object, list[dict]] = {}
            for row in self.tables[entity]:
                if field_name in row:
                    index.setdefault(row[field_name], []).append(row)
            self._indices[key] = index
        return self._indices[key]

    def select(self, entity: str, where: dict) -> list[dict]:
        """Rows of `entity` matching the fully resolved predicate `where`."""
        if entity == "StockMovement":
            rows = self.movements(where["sku"], where["loc"])
        else:
            # Narrow by the first equality field through an index, then filter: a
            # linear scan of a 12165-row table per parent group is the descriptor
            # script's whole runtime otherwise.
            equality = next((f for f, v in where.items() if not isinstance(v, dict)), None)
            rows = (self._index(entity, equality).get(where[equality], [])
                    if equality is not None else self.tables[entity])
        return [row for row in rows if matches(row, where)]

    def sample_order_line(self, rng: Rng) -> tuple[str, int, str]:
        """The anchor `(order_no, line_no, sku)` of one existing order line."""
        order = self.tables["Order"][rng.below(self.n_orders)]
        line = self.lines_by_order[order["order_no"]][rng.below(
            len(self.lines_by_order[order["order_no"]]))]
        return order["order_no"], line["line_no"], line["sku"]

    def sample_lot(self, rng: Rng) -> str:
        return self.tables["Lot"][rng.below(self.n_lots)]["lot_no"]

    def sample_movement(self, rng: Rng, zipf: Zipf, locs: list[str],
                        attempts: int = 8) -> tuple[str, str, int]:
        """The anchor `(sku, loc, since_day)` of one existing supply movement.

        Bounded attempts keep the draw deterministic while skipping a lost sku that
        has no supply row to anchor an as-of join on.
        """
        sku, loc = sku_name(zipf.draw()), rng.pick(locs)
        for _ in range(attempts):
            rows = self.movements(sku, loc)
            if rows:
                return sku, loc, rows[rng.below(len(rows))]["day"]
            sku, loc = sku_name(zipf.draw()), rng.pick(locs)
        return sku, loc, 1


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
        self.seed = seed
        self.n_requests = n_requests
        self.n_customers = n_customers
        self.n_sku = n_sku
        self.n_loc = n_loc
        self.theta = theta
        self._world: JoinWorld | None = None
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

    # -- join-heavy shapes (M2 / ticket 81) --------------------------------
    #
    # Each is a read-only request in the same JSONL dialect as the flat flow: a
    # named block DAG whose later blocks bind `$block.field` from an earlier
    # block's rows (the QBE join trick in `docs/contracts.md`). A multi-row parent
    # therefore makes the child predicate a *set* predicate, which is exactly the
    # fan-out these shapes exist to expose. The engine adapter
    # (`engine/src/request.rs`) accepts none of them today: `validate` demands the
    # `cust -> take` graph and `read_shape` resolves only a single Customer probe,
    # so the request-level execution of these shapes is unmeasured, not measured.

    @property
    def world(self) -> JoinWorld:
        """The modelled data the join shapes address; built only when they are used."""
        if self._world is None:
            self._world = JoinWorld(self.seed, n_sku=self.n_sku,
                                    n_customers=self.n_customers, n_locs=self.n_loc,
                                    theta=self.theta)
        return self._world

    def star_fanout(self) -> dict:
        """Replenishment / days-of-cover: order line -> product -> supplier, plus the
        sku's supply window at the location. Hops: line (1) -> product (2) -> supplier
        (3) and line (1) -> movements (2)."""
        order_no, line_no, sku = self.world.sample_order_line(self.rng)
        loc = self.rng.pick(self.locs)
        day = 1 + (int(order_no[1:]) - 1) // self.world.orders_per_day
        return {
            "id": self._next_req_id(),
            "class": {"durability": "batched", "max_staleness": 5},
            "params": {"order_no": order_no, "line_no": line_no, "sku": sku, "loc": loc,
                       "since_day": max(1, day - 29)},
            "blocks": {
                "line": {"get": {"OrderLine": {"where": {"order_no": "$order_no",
                                                          "line_no": "$line_no"}}}},
                "product": {"needs": ["line"],
                            "get": {"Product": {"where": {"sku": "$line.sku"}}}},
                "supplier": {"needs": ["product"],
                             "get": {"Supplier": {"where": {"supplier_no":
                                                              "$product.supplier_no"}}}},
                "movements": {"needs": ["line"],
                              "find": {"StockMovement": {
                                  "where": {"sku": "$line.sku", "loc": "$loc",
                                            "day": {"$gte": "$since_day"}},
                                  "order": ["movement_no"], "limit": 128}}},
            },
        }

    def multi_hop(self) -> dict:
        """Recall / traceability: lot -> products -> order lines -> orders -> customers,
        plus the orders' returns. Hops: items (1) -> products (2) -> lines (3) -> orders
        (4) -> buyers (5), and lines (3) -> returns (4)."""
        lot_no = self.world.sample_lot(self.rng)
        return {
            "id": self._next_req_id(),
            "class": {"durability": "batched", "max_staleness": 30},
            "params": {"lot_no": lot_no},
            "blocks": {
                "items": {"find": {"LotItem": {"where": {"lot_no": "$lot_no"}}}},
                "products": {"needs": ["items"],
                             "find": {"Product": {"where": {"sku": "$items.sku"}}}},
                "lines": {"needs": ["products"],
                          "find": {"OrderLine": {"where": {"sku": "$products.sku"}}}},
                "orders": {"needs": ["lines"],
                           "get": {"Order": {"where": {"order_no": "$lines.order_no"}}}},
                "buyers": {"needs": ["orders"],
                           "get": {"Customer": {"where": {"email": "$orders.buyer_email"}}}},
                "returns": {"needs": ["lines"],
                            "find": {"Return": {"where": {"order_no": "$lines.order_no"}}}},
            },
        }

    def as_of(self) -> dict:
        """As-of join: the supply facts of one `(sku, loc)` in a window, each joined to
        the SCD-2 product version valid at its own day `t`, on `(sku, t)`. Hops:
        facts (1) -> versions (2)."""
        sku, loc, day = self.world.sample_movement(self.rng, self.zipf, self.locs)
        return {
            "id": self._next_req_id(),
            "class": {"durability": "batched", "max_staleness": 2},
            "params": {"sku": sku, "loc": loc, "since_day": max(1, day - 9)},
            "blocks": {
                "facts": {"find": {"StockMovement": {
                    "where": {"sku": "$sku", "loc": "$loc",
                              "day": {"$gte": "$since_day"}},
                    "order": ["movement_no"], "limit": 64}}},
                "versions": {"needs": ["facts"],
                             "find": {"ProductVersion": {
                                 "where": {"sku": "$facts.sku",
                                           "valid_from": {"$lte": "$facts.day"},
                                           "valid_to": {"$gt": "$facts.day"}}}}},
            },
        }

    def anti_join(self) -> dict:
        """Lost sales: a day's demanded skus, each probed for a supply movement at one
        location. The probe's *absence* is the answer. Hops: demand (1) -> supply (2).

        Marking the lost skus into a `LostSale` row would need an aggregate-to-write
        shape (`docs/contracts.md`, L-07) that does not exist, so the request stops at
        the detection half and the absence is read from `supply.status == empty`.
        """
        day = 1 + self.rng.below(self.world.n_days)
        loc = self.rng.pick(self.locs)
        return {
            "id": self._next_req_id(),
            "class": {"durability": "batched", "max_staleness": 30},
            "params": {"day": day, "loc": loc},
            "blocks": {
                "demand": {"find": {"OrderLine": {"where": {"day": "$day"},
                                                   "order": ["sku"], "limit": 512}}},
                "supply": {"needs": ["demand"],
                           "find": {"StockMovement": {
                               "where": {"sku": "$demand.sku", "loc": "$loc",
                                         "delta": {"$gt": 0}},
                               "limit": 1}}},
            },
        }

    def join_requests(self, per_shape: int) -> Iterator[dict]:
        """`per_shape` requests of every join-heavy shape, round-robin in JOIN_NAMES order."""
        assert per_shape >= 1
        for _ in range(per_shape):
            for name in JOIN_NAMES:
                yield self.shape(name)

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
    ap.add_argument("--shapes", choices=("mix", "join"), default="mix",
                    help="mix: the flat inventory flow; join: the four join-heavy shapes")
    ap.add_argument("--per-shape", type=int, default=25,
                    help="--shapes join only: requests emitted per join-heavy shape")
    ap.add_argument("--out", default="-")
    a = ap.parse_args(argv)
    g = Generator(a.seed, a.requests, a.customers, a.skus, a.locs, a.theta)
    reqs = g.requests() if a.shapes == "mix" else g.join_requests(a.per_shape)
    out = sys.stdout if a.out == "-" else open(a.out, "w", encoding="utf-8", newline="\n")
    for line in render(reqs):
        out.write(line + "\n")
    if out is not sys.stdout:
        out.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
