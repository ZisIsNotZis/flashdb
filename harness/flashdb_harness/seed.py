"""Inventory dimension seeding trace (Slice 18).

The request adapter (`engine/src/request.rs`) deliberately has **no create/seed
shape**: the four supported templates only read, or run `cust -> take` against
dimensions that must already exist (its header: "no seed/schema endpoint is
provided here"). So the inventory flow replays `cust: empty` / `take: skipped`
against a fresh engine and the WAL never grows. This module emits the missing
piece: a *seeding trace* that `flashdb-scale seed-inventory` applies as raw
engine P/U ops (the same way `flashdb-scale seed` and the engine tests bootstrap
their data).

Trace format -- one JSON object per line, each line is one committed block::

    {"block":"seed-00000000","docs":[
      {"entity":"Customer","doc":{"email":"c000000@example.com"},
       "unique":{"field":"email","value":"c000000@example.com"}},
      {"entity":"Stock","doc":{"sku":"S000001","loc":"L000","on_hand":1000000},
       "unique":{"field":"sku_loc","value":["S000001","L000"]}}
    ]}

Rules, which must match `engine/src/request.rs`'s `Catalog` exactly (the applier
enforces them, see `cmd_seed_inventory` in `engine/src/bin/flashdb-scale.rs`):

- `Customer`: declared unique field `email`; unique value is the email string.
  The adapter probes it as `unique_lookup("Customer", "email", email.as_bytes())`.
- `Stock`: declared unique field `sku_loc`; unique value is the JSON array
  `[sku, loc]`. The adapter probes it as
  `unique_lookup("Stock", "sku_loc", serde_json::to_vec(&[sku, loc]))`.
- Neither dimension declares an `x-ref`, so no `R` reverse entries are seeded;
  the flow writes its own `Order.buyer` / `StockMovement.stock` reverse entries.
- Every doc is paired with its declared unique in the same block, so P and U
  are always consistent (the adapter cross-checks the two).

Determinism: only the harness `Rng` (SplitMix64) is used -- no `random`, no
floats -- so the same parameters produce byte-identical traces anywhere.
"""

from __future__ import annotations

import argparse
import sys
from typing import Iterator

from flashdb_harness.generator import Rng, email_name, loc_name, render, sku_name


class SeedGenerator:
    """Deterministic Customer + Stock dimension rows for the inventory flow."""

    def __init__(
        self,
        seed: int = 1,
        n_customers: int = 5_000,
        n_sku: int = 10_000,
        n_loc: int = 8,
        on_hand: int = 1_000_000,
        on_hand_jitter: int = 0,
        block_docs: int = 1_000,
    ) -> None:
        assert n_customers >= 1 and n_sku >= 1 and n_loc >= 1
        assert on_hand >= 1 and on_hand_jitter >= 0
        assert block_docs >= 1
        self.rng = Rng(seed)
        self.n_customers = n_customers
        self.n_sku = n_sku
        self.n_loc = n_loc
        self.on_hand = on_hand
        self.on_hand_jitter = on_hand_jitter
        self.block_docs = block_docs

    def customer_docs(self) -> Iterator[dict]:
        for i in range(self.n_customers):
            email = email_name(i)
            yield {"entity": "Customer", "doc": {"email": email},
                   "unique": {"field": "email", "value": email}}

    def stock_docs(self) -> Iterator[dict]:
        # Ascending sku rank then loc, matching the flow's `S{rank:06d}` /
        # `L{j:03d}` naming so every (sku, loc) the flow probes exists.
        for rank in range(1, self.n_sku + 1):
            sku = sku_name(rank)
            for j in range(self.n_loc):
                loc = loc_name(j)
                count = self.on_hand + self.rng.below(self.on_hand_jitter + 1)
                yield {"entity": "Stock",
                       "doc": {"sku": sku, "loc": loc, "on_hand": count},
                       "unique": {"field": "sku_loc", "value": [sku, loc]}}

    def blocks(self) -> Iterator[dict]:
        docs: list[dict] = []
        index = 0
        for doc in [*self.customer_docs(), *self.stock_docs()]:
            docs.append(doc)
            if len(docs) == self.block_docs:
                yield {"block": f"seed-{index:08d}", "docs": docs}
                index += 1
                docs = []
        if docs:
            yield {"block": f"seed-{index:08d}", "docs": docs}


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="emit an inventory dimension seeding trace")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--customers", type=int, default=5_000)
    ap.add_argument("--skus", type=int, default=10_000)
    ap.add_argument("--locs", type=int, default=8)
    ap.add_argument("--on-hand", type=int, default=1_000_000)
    ap.add_argument("--on-hand-jitter", type=int, default=0)
    ap.add_argument("--block-docs", type=int, default=1_000)
    ap.add_argument("--out", default="-")
    a = ap.parse_args(argv)
    g = SeedGenerator(a.seed, a.customers, a.skus, a.locs,
                      a.on_hand, a.on_hand_jitter, a.block_docs)
    out = sys.stdout if a.out == "-" else open(a.out, "w", encoding="utf-8", newline="\n")
    for line in render(g.blocks()):
        out.write(line + "\n")
    if out is not sys.stdout:
        out.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
