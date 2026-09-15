# flashdb — prior art and positioning

Budget: 150 lines / 24,000 chars

**Verification status: partial.** Bibliographic checks were run against Crossref on 2026-09-15 (dblp rate-limits aggressively; Crossref is the practical route from this machine). Nine entries are now **verified**, one author list was **wrong and is corrected**, and two entries were **deleted** because the cited name either does not denote a database system or denotes a different one.

| Status | Items |
|---|---|
| **Verified** | Kohn, Leis, Neumann, *Adaptive Execution of Compiled Queries*, ICDE 2018 — the earlier attribution to "Menon et al." was **wrong**. H2O (Alagiannis, Idreos, Ailamaki, SIGMOD 2014). Monkey (Dayan, Athanassoulis, Idreos, SIGMOD 2017). BtrBlocks (Kuschewski, Sauerwein, Alhomssi, Leis et al., SIGMOD/PACMMOD 2023) — the earlier author list "Kuschewski, Sauer, Neumann, Freitag" was **wrong**. FSST (Boncz, Neumann, Leis, PVLDB 2020). Harinarayan, Rajaraman, Ullman (SIGMOD 1996; reprinted 1999). Amazon Redshift Re-invented (Armenatzoglou et al., SIGMOD 2022) — the paper is confirmed; that it describes Automatic Table Optimization is **not**. |
| **Deleted** | **"ArcaDB"** — it exists, but it is a 2024 disaggregated query engine (Ruiz-Rohena, Rodríguez-Martínez, IEEE 2024), **not** adaptive-layout prior art. An earlier revision of this repository paired it with NoDB in the cracking lineage, which was simply wrong. **"Tesseract"** — no database system of that name was found; the only match is the dictionary word. Neither the author nor the reviewer could identify it, so an unverifiable name was removed rather than carried. |
| **Still unverified** | SageDB (name and year corroborated, but CIDR is not indexed in Crossref, so the venue is unconfirmed). Napa, VLDB 2021. Data Blocks, SIGMOD 2016. Fractured Mirrors. Dostoevsky. Agrawal, Chaudhuri, Narasayya, VLDB 2000. "LSM-bush". Oracle Automatic Indexing. Snowflake automatic clustering, BigQuery automatic re-clustering, Databricks liquid clustering (product features). Umbra, CIDR 2020. |

Everything below was recalled from memory and may still be wrong in author, year or venue. Entries the review could not confirm are marked **[unverified]**. Verification continues in `.scratch/02-citation-verification/`. **Do not cite anything from this file as fact.**

This file exists because an earlier single-table version marked as "open" several things that are shipped features, and ended with a novelty claim that its own table contradicted.

## What is established — and therefore not a contribution

| Area | Established by | What that means here |
|---|---|---|
| Continuous, background, budgeted re-layout with no downtime | Snowflake automatic clustering; BigQuery automatic re-clustering; Redshift Automatic Table Optimization; Databricks liquid clustering (product features; no paper located **[unverified]**) | "no migration project" and "continuous drift" are a product category, not a contribution |
| Fine-grained adaptive layout created from the query mix, raw data as fallback | **H2O** (Alagiannis, Idreos, Ailamaki, SIGMOD 2014) | **the closest work in existence to the core mechanism.** Per-column, per-chunk layouts created and dropped continuously from the observed mix, with a cost model deciding. The delta must be stated explicitly or this design has no claim here |
| Adaptive indexing from zero; drift | database cracking (Idreos, Kersten, Manegold, CIDR 2007); updating a cracked database (SIGMOD 2007); stochastic cracking (VLDB 2011) | "learn from zero" and "drift handling" are established |
| Multiple physical layouts of one logical table, chosen per query, base copy as fallback | **Fractured Mirrors** (Ramamurthy, DeWitt, Su, VLDB 2002); C-Store WS/RS with epoch validation (Stonebraker et al., VLDB 2005) | thesis claim 1 in miniature — the design cited C-Store only for compression, its least relevant contribution |
| View/index selection; incremental view maintenance | Harinarayan, Rajaraman, Ullman (SIGMOD 1996); Gupta (ICDT 1997); Agrawal, Chaudhuri, Narasayya (VLDB 2000); Gupta & Mumick (1995); DBToaster (SIGMOD 2012) | "layout is a materialized view" renames an NP-hard classic; the hardness and its approximation literature must be engaged or the per-tile restriction must be shown to escape it |
| Workload-driven candidate search with what-if replay | AutoAdmin what-if analysis (Chaudhuri & Narasayya, SIGMOD 1998); DTA (Agrawal et al., VLDB 2000); DB2 Design Advisor (Zilio et al., VLDB 2004); workload compression (Chaudhuri, Gupta, Narasayya, SIGMOD 2002) | the "trace-replay oracle" is the advisor lineage; workload compression is the missing technique that makes replay tractable |
| Automatic tuning with validation and automatic rollback | Oracle Automatic Indexing; SQL Server Automatic Tuning and Automatic Plan Correction **[unverified]** | funded, proven, canaried promotion with rollback (I11) is shipped product discipline; the only untested part is that the funding is *one shared* budget across heterogeneous decision types |
| Per-chunk automatic encoding selection | **BtrBlocks** (Kuschewski, Sauerwein, Alhomssi, Leis et al., SIGMOD 2023 — **verified**, and the author list in an earlier revision was wrong); Parquet and ORC writers | "learned per-tile encoding" is narrower than claimed — data-statistic-driven selection already exists; the untested delta is *workload*-driven encoding chosen jointly with clustering |
| Self-contained block formats with offset directories | Parquet; ORC; LevelDB/RocksDB SSTables; Arrow IPC; FSST for strings (Boncz, Neumann, Leis, PVLDB 2020 — **verified**) | "reorg is a memcpy" and per-block directories are format properties, not primitives |
| One scalar cost objective, per-component policy, one shared budget | **Monkey** (Dayan, Athanassoulis, Idreos, SIGMOD 2017); **Dostoevsky** (Dayan, Idreos, SIGMOD 2018) | the shared-budget-with-expected-cost structure is theirs. "LSM-bush" is **[unverified]** as a title. Their optimality derives from level-size ratio and run structure, which tiled storage does not have, so "the same idea" is not licensed — only generic budget allocation transfers |
| Joint representation and execution specialization | **Data Blocks** (Lang, Mühlbauer, Funke, Boncz, Neumann, Kemper, SIGMOD 2016); vectorization-vs-compilation (Sompolski, Zukowski, Boncz, DaMoN 2011; Kersten, Leis, Kemper, Neumann, VLDB 2018) | "layout + code jointly" is established; the residual is only that *the same offline profile drives both*, with the reorg→plan-invalidation coupling priced |
| Adaptive plan specialization against a profile | Kohn, Leis, Neumann, *Adaptive Execution of Compiled Queries*, ICDE 2018 — **verified**; the Menon et al. CMU work is *Relaxed Operator Fusion*, ICDE 2017, a different paper | per-pattern specialization is established |
| if-conversion and branchless execution | Allen, Kennedy, Porterfield, Warren (POPL 1983); MonetDB/X100 selection vectors (Boncz, Zukowski, Nes, VLDB 2005) | predication is 40 years old. The database-specific observation — that the guard is itself an IO, so if-conversion is worth more here — is an instantiation, not a technique |
| SLO operation; penalty methods; queueing law | Google SRE book (Beyer et al., 2016); penalty methods for constrained optimisation; M/M/1 and Kingman response laws; tail latency (Dean & Barroso, CACM 2013) | `objective.md` already concedes angriness is an error budget renamed, so "error budget as the objective function" cannot be listed as open |
| Learned database systems | **SageDB** (Kraska et al., CIDR 2019); learned index structures (Kraska, Beutel, Chi, Dean, Polyzotis, SIGMOD 2018); self-driving DBMS (Ma, Pavlo et al., CIDR 2017); OtterTune (Van Aken, Pavlo, Gordon, Cabrera, SIGMOD 2017) | **this is the thesis's actual neighbourhood.** SageDB already argues for synthesising layout, index and execution from the workload via learned models. A design claiming learned physical layout without engaging it reads as uninformed rather than novel |
| Off-policy evaluation and bandits | Precup, Sutton, Singh (2000); Dudík, Langford, Li (2011); UCB and EXP3 (Auer et al., 2002) | `learning.md` calls off-policy evaluation the core methodological risk and names no method |
| Trace-replay depth | Napa (VLDB 2021) — maintains multiple physical layouts and reorganises in the background under a cost model **[unverified as to exact mechanisms]** | if accurate, this row is nearly empty rather than open |
| Anti-join / interval-join execution | (not researched) | relevant because two of the five frozen benchmark workloads may not be expressible in the frozen grammar (`review-02` F11) |

## What may still be unclaimed

Three things survive, and all three are claims about **method and measurement**, not about mechanisms:

1. **A per-tile learned (bandit or RL) layout policy**, as opposed to an analytic or sampling heuristic selector — Snowflake, Redshift, H2O and BtrBlocks all choose by cost model or data statistics rather than by learning from realised outcomes.
2. **Sufficiency and unification of one SLO-capability-anchored scalar penalty** across heterogeneous decision types — layout, encoding, speculation, compilation — i.e. one shared budget rather than four per-type budgets. This is a genuine and testable claim, and the counterfactual is four independent budgets.
3. **Grammar-enforced canonicity** making the learning key exact by construction, rather than a best-effort digest. The baseline to beat is constant-stripping (`pg_stat_statements`-style), and the metric is the pattern-shatter rate on real client behaviour.

Everything else — continuous budgeted re-layout, per-chunk encoding selection, view and index selection, incremental maintenance, if-conversion, plan specialization, error budgets, background reorganisation under a cost model — is established prior work.

## The claim, demoted

> For a single-node IO-bound workload in the ~10 TB / ~128 GB regime, a single SLO-capability-anchored scalar penalty, scored by a trace-replay cost model, is **sufficient** to drive a per-tile learned layout policy — jointly with encoding, speculation and compilation — to within a measured regret of an oracle layout at equal durability, while grammar-enforced canonicity makes the learning key exact rather than a best-effort digest. Every component is established prior work. **The contribution is a sufficiency-and-unification claim, it is empirical, and it is falsifiable** — and if the measured gain over the `flashdb-declared` and `flashdb-static-derived` controls (`dev-loop.md`) is not material, the claim is dead.

The previous formulation — that *nothing* combines these — was false as written and contradicted the table directly above it in `design.md`.

## Verification and reading order

1. **Read H2O (SIGMOD 2014) and SageDB (CIDR 2019) first**, before any implementation work. They are the two closest works and together they decide whether the remaining claim is meaningful. H2O is verified and available; SageDB's venue is unconfirmed but the work is known to exist.
2. Continue the remaining unverified items in `.scratch/02-citation-verification/`: Napa's mechanisms, Data Blocks, Fractured Mirrors, Dostoevsky, Agrawal et al. VLDB 2000, "LSM-bush", Oracle Automatic Indexing, and the Snowflake/BigQuery/Databricks features.
3. Add the ledger ablation: one shared budget versus four independent budgets. This is the only part of the claim that is neither established prior work nor already refuted, and it is measurable.
