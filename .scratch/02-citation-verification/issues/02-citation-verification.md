# 02-citation-verification — verify or delete every prior-art citation

Status: claimed (partially resolved)
Blocked by: nothing. `dblp.org` answers but rate-limits aggressively (429, then connection resets, at ~16 rapid queries); Crossref's REST API works and is the practical verification route from this machine. Seven of the items below are resolved.
Ticket: this file. Decision ticket — question in, decision out.

## Question

Every prior-art claim in this repository was written from memory. `docs/prior-art.md` marks the unconfirmed ones `[unverified]`, but an unverified citation cannot bound novelty, and at least one entry from an earlier revision (a Google system called "Tesseract") was removed because **neither the author nor the reviewing agent could identify it**. Until the items below are resolved, `docs/prior-art.md` and every novelty sentence in `docs/design.md` and `README.md` are provisional.

## Items to resolve

| # | Item | Question |
|---|---|---|
| 1 | Kohn, Leis, Neumann, *Adaptive Execution of Compiled Queries*, ICDE 2018 | Is this the correct attribution? An earlier revision said "Menon, Leis, Neumann", which appears to be a conflation with Menon, Mowry, Pavlo, *Relaxed Operator Fusion*, ICDE 2017 (CMU). |
| 2 | Napa, VLDB 2021 | Does it maintain multiple physical layouts of one table, reorganise them in the background under a cost model, and let the planner choose? If yes, the "block-level adaptive clustering" claim in this repository is nearly empty. |
| 3 | Snowflake automatic clustering | Is there a peer-reviewed description, or is it product documentation only? Dageville et al. SIGMOD 2016 covers micro-partitions, not automatic clustering. |
| 4 | "ArcaDB" | Does this system exist as published work? It was cited in an earlier revision next to NoDB. If it cannot be confirmed, delete it. NoDB (Alagiannis et al., SIGMOD 2012) is about querying raw external files, not layout adaptation, so pairing them muddled the lineage. |
| 5 | "LSM-bush" | Exact title, authors, venue. Monkey (SIGMOD 2017) and Dostoevsky (SIGMOD 2018) are confirmed. |
| 6 | Amazon Redshift Automatic Table Optimization | Is it described in "Amazon Redshift Re-invented", SIGMOD 2022, or only in product documentation? |
| 7 | Umbra's variable-length representation / compressed tuple identifiers | Confirm the specific paper and mechanism. "Neumann et al." is not a citation; Neumann & Freitag, CIDR 2020 is the likely one. |
| 8 | Canonical citation for speculative prefetching in query execution | The reviewing agent deliberately declined to invent one. If none exists, `learning.md`'s speculation section must say so rather than implying a lineage. |

## Results (Crossref, 2026-09-15)

| # | Item | Result |
|---|---|---|
| 1 | Kohn, Leis, Neumann, ICDE 2018 | **Verified.** The earlier "Menon, Leis, Neumann" attribution was wrong, as the reviewer suspected. Menon et al. is *Relaxed Operator Fusion*, ICDE 2017 (CMU) — a different paper. |
| 4 | "ArcaDB" | **Deleted, and the error was worse than "unconfirmed".** ArcaDB exists — 2024, IEEE, *ArcaDB: A Disaggregated Query Engine for Heterogeneous Computational Environments* (Ruiz-Rohena, Rodríguez-Martínez). It is **not** adaptive-layout prior art. An earlier revision of this repository paired it with NoDB inside the database-cracking lineage, which was simply wrong. |
| 5 | "LSM-bush" | **Still unverified.** Not found. Monkey (Dayan, Athanassoulis, Idreos, SIGMOD 2017) **verified**; Dostoevsky not found under that query. |
| 6 | Redshift Automatic Table Optimization | **Paper verified, claim not.** *Amazon Redshift Re-invented* (Armenatzoglou et al., SIGMOD 2022) exists; whether it describes ATO is unconfirmed. |
| 7 | Umbra variable-length representation | **Still unverified.** Not found. |
| — | BtrBlocks | **Verified, and the author list was wrong.** Kuschewski, Sauerwein, Alhomssi, Leis et al., SIGMOD/PACMMOD 2023 — not "Kuschewski, Sauer, Neumann, Freitag". |
| — | FSST | **Verified.** Boncz, Neumann, Leis, PVLDB 2020. |
| — | H2O | **Verified.** Alagiannis, Idreos, Ailamaki, SIGMOD 2014. |
| — | Harinarayan, Rajaraman, Ullman | **Verified** (SIGMOD 1996; Crossref returns the 1999 reprint volume). |
| — | "Tesseract" | **Deleted.** The only match is the dictionary word; no database system of that name was found, by the author or by the reviewing agent. |
| 8 | Speculative prefetching in query execution | **Still unresolved.** If no canonical paper exists, `learning.md` must say so rather than implying a lineage. |

Still open, all in `docs/prior-art.md` as `[unverified]`: SageDB's CIDR venue, Napa's mechanisms, Data Blocks, Fractured Mirrors, Dostoevsky, Agrawal/Chaudhuri/Narasayya VLDB 2000, Oracle Automatic Indexing, and the Snowflake/BigQuery/Databricks product features.

## Comments

- 2026-09-15 — pi / claude — Opened as `deferred (blocked: no network)`, then **immediately corrected**: a probe showed `dblp.org` and `sigmod.org` returning 200, so the block was assumed rather than measured. Crossref queries then resolved seven items and found four errors — including the two the novelty review flagged as likely (the Kohn/Menon misattribution) and the ArcaDB mis-pairing, which the reviewer could only call "unconfirmed". Lesson for the record: **probe the environment before recording a blocker**.
- 2026-09-15 — pi / claude — Remaining items need either a reached-paper check (SageDB, Napa, Data Blocks, Dostoevsky) or a product-documentation check (Snowflake, BigQuery, Databricks, Oracle). Neither is a Crossref query.

## Acceptance criteria

1. Every entry in `docs/prior-art.md` either carries a verified citation or is marked `[unverified]` with the reason.
2. Items 1, 4, 5 and 6 are resolved as verified, corrected, or deleted.
3. H2O and SageDB have been read, and the delta claimed against each is stated in one sentence that a reader of those papers would accept.
4. Any novelty sentence that the verification invalidates is narrowed or removed from `design.md`, `README.md` and `prior-art.md` in the same change.

## Not in scope

Finding new prior art beyond the items above. This ticket resolves citations; it does not re-run the novelty review.
