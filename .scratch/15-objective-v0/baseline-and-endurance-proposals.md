# Proposals for the author: external baselines, and endurance as a constraint (2026-10-06)

Both are proposals, not decisions. Neither has been used in a run, so adding them now does
not invalidate anything (the pre-registration's rule is that changing a number *after seeing
results* invalidates the run).

## 1. The bar the author stated

Paraphrased from the author: on ordinary low load the engine must not be slower than SQLite
or PostgreSQL; on high load it must beat them by a lot; given enough learning time it should
beat them; and the **initial** policy (before any learning) must not be absurdly bad - 10x
slower than PostgreSQL is unacceptable.

This is an **external** claim. The current pre-registration is entirely **internal**: T1-T4
compare learned against declared / static-derived / oracle. Both matter, but the external one
is what decides whether the design is worth doing, and it is currently untested.

## 2. Proposal: baselines as arms (B1)

| arm | config |
|---|---|
| `sqlite` | WAL enabled, `synchronous` stated explicitly, one writer connection |
| `postgres` | a frozen, disclosed config - no tuning race |
| `lmdb`, `rocksdb` | named in the design log as the honest early comparisons |
| `flashdb-declared` | the shipped default = the initial policy |
| `flashdb-learned` | after a stated learning period |

Durability must be matched as closely as each product allows (fsync per commit vs group
commit), and the difference disclosed. All arms are driven through one harness at the request
level, because flashdb has no SQL and its payload is opaque to the engine.

Honesty requirements: the instance must be in the same memory-constrained regime for every
arm (Postgres must not get a buffer pool that holds the whole dataset while flashdb is cold),
and the same latency rule must be applied to the baselines when computing their sustained
level.

## 3. Proposal: three new thresholds

| id | threshold | proposal |
|---|---|---|
| **T5** | low-load non-inferiority | at the 1x rung, flashdb <= **1.5x** the slower of {sqlite, postgres}; **2x** is the fail line; a 10x gap is a design failure |
| **T6** | high-load superiority | at the highest common offered level, flashdb >= **3x** the best baseline's sustained throughput, under the same latency rule |
| **T7** | initial-policy quality | the **declared** arm alone must already pass T5, i.e. "the initial policy must not be absurd" becomes a number on a design deliverable |

## 4. Proposal: endurance becomes a constraint, not a price (the lambda question)

Measured: on the inventory workload the reorganisation write term is 0.083-0.669% of the
service-time term (and ingest only 0.003%). A price of that size cannot move any argmin
however lambda is swept. That is not a calibration defect - it says endurance is **not the
scarce resource on this workload**; device service time is.

Recommendation:

1. **Remove lambda from the reward.** Keep the reward as service time (the resource that is
   actually scarce) and make endurance a **hard budget**: refuse a layout switch whose
   projected reorganisation rate exceeds the remaining per-window budget.
2. **This machinery already exists and already worked**: the churn control added in slice 39
   is exactly a per-window reorganisation budget, and it cut the learner's reorganisation
   bytes from 402,984,851 to 28,191,940 (14x). Reuse it rather than inventing a price.
3. **Report the capacity-versus-budget curve** instead of a lambda: "under a reorganisation
   budget of B bytes/window, sustained throughput is X". That is the question a user with a
   real device endurance limit (DWPD/TBW) actually has, and it is answerable.
4. Keep a very small lambda only to break ties, and say so.
5. This is also consistent with the pre-registration, which already states endurance as a
   **threshold** (T3: reorganisation <= 3x declared), not as a traded quantity.

The general form: **maximise sustained load subject to (latency ceiling) and (write-amplitude
budget)**. Both constraints are hard; neither is a price.
