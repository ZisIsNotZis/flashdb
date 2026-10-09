# 40 - the unit of a deferred constraint check (finding, 2026-10-06)

## The author's confirmed semantics (2026-10-06)

A request holds N blocks; **a block is the atomic unit**; a block may contain several
operations where *each on its own violates a constraint but together they do not* - that is
legal, so constraint checking is **after the fact** (on the block's post-state), not per
operation.

## What the engine already does

This is implemented. `Engine::commit_batch` accumulates a per-prefix overlay
(`BatchUnique { owners: HashMap<handle, bool>, .. }`, later ops win) and validates
uniqueness against the **projected post-state**: the published owners overlaid with the
batch's own net effect. So a block that deletes owner A and adds owner B for the same
unique prefix is allowed. Within a block, "each op violates, together they don't" works.

## The finding: the projection unit is the whole commit group, not the block

`BatchOutcome::Conflict`'s own doc says the loser is the block that "leaves more than one
live owner **in the batch's projected post-state**". The batch is not a request:

- `flashdb-load`'s grouped path (`--group-requests N`, the real inventory `cust -> take`
  flow, slice 23) calls **one** `commit_batch(&blocks)` with blocks from **N independent
  business requests** (`end = index + size`, one block per request pushed into `blocks`).

Three consequences:

1. **The verdict depends on grouping.** Request B (insert a row whose unique value X is
   owned by A) succeeds when it happens to share a group with request A's delete of X, and
   gets `unique_violation` when it arrives alone. Both orders are serializable, so this is
   not a correctness violation - but the client cannot reason about its own request alone,
   and the same request can return `ok` or `failed` on different runs.
2. **One conflict rejects the whole group.** `commit_batch` is all-or-nothing by
   construction (it refuses before appending), so a single conflicting request makes its
   N-1 unrelated neighbours retry. Under a hot unique key - and the inventory `P|Stock` row
   is exactly hot - that is a head-of-line amplification tax that grows with the group size,
   which is the very lever slice 23 used to reach 4,232 rps.
3. **It contradicts the confirmed contract in the *validation* dimension.** `contracts.md`
   says the group is NOT atomic, blocks are independent, and only a durable *prefix* is
   promised. The durability dimension honours that; the validation dimension does not.

## Why this did not show up in the 0b reports

Every 0b run reports `commit = 1.000`, so no conflict occurred: the generated flow does not
contend on one SKU. The defect is latent until the workload has a hot unique key, which is
exactly the realistic inventory case (Zipfian popularity was part of the original pitch).

## Recommendation (needs the author's nod on the unit)

Validate **per block** - the unit the author has now confirmed: a block's ops are projected
together (deferred, post-state), and distinct blocks are validated independently against
published state plus the effects of *declared* dependencies. Then, instead of rejecting the
whole group, **drop only the conflicting request from the batch** and commit the rest.

Note that the two mechanisms are different and both are needed:

- **Engine-known constraints** (unique keys, including the derived `U|`/`R|` families) get
  the deferred post-state check described here.
- **Business invariants** the engine cannot see (the document payload is opaque to it) -
  "on_hand >= 0", double-entry balance - are already handled by the rule that a block's
  `when` guard belongs to the whole block and rolls back every write in it. That is the
  author's "together they are fine" pattern at the business level, and it needs no change.

Also consistent with existing intent: the front-end already rejects same-key writes with no
declared edge rather than guessing; collisions on the same unique prefix within one request
should be rejected the same way.

## Tests to add (not yet written)

1. One block: two rows swap a value under a unique key (each alone violates, together fine).
2. One block: delete then re-insert the same unique value with a new handle.
3. **Grouping independence**: two independent requests in one group, one deleting and one
   inserting the same unique value; assert the verdict is the same as when they run alone.
4. **No collateral damage**: a group of N requests where one conflicts; assert the other
   N-1 commit.
