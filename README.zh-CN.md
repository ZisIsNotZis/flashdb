[English](README.md) | **简体中文**

<p align="center"><img src="assets/logo.svg" alt="flashdb" width="200"></p>

# flashdb

*一个单机、日志结构（log-structured）的文档存储引擎：每个区域的物理布局是从工作负载里学出来的，而不是让用户先声明。*

**在数据还不存在时先声明布局，是一个要用实例的整个生命周期来还的猜测：页大小、合并阈值、要不要建二级索引，这些通常都由引擎在开头一次定死，而选错的那一个要等到代价显现才会被发现。flashdb 观察真实的请求轨迹，在运行时按键空间（keyspace）的每个区域重新做这些选择，让布局跟着数据走。**

它是一个研究原型，核心论点正在被检验，不是一个可以拿来用的数据库：用来裁决它的那次对比，在单机上对着 sqlite 和 postgres 打过一遍，结果是输了（见[论点的现状](#论点的现状)）。

**它不适用于：** 生产数据、多节点部署、SQL，以及除了仓库里那条固定 inventory 轨迹以外的任何负载——请求适配层只接受一种块图（block graph），`cust → take`（见[目前没有的](#目前没有的)）。

## 快速上手

从一次干净的克隆开始，Rust 1.98.1。下面的构建命令把 crate 源固定到 aliyun 镜像并加 `--offline`，这是仓库自己的运行脚本所用的封闭形式——不联网，也不依赖宿主机的 cargo 配置：

```sh
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      build --workspace --offline
./target/debug/flashdb-scale seed /tmp/flashdb-demo.db 20000 500 256
./target/debug/flashdb-scale verify /tmp/flashdb-demo.db 20000
./target/debug/flashdb-scale checkpoint /tmp/flashdb-demo.db
./target/debug/flashdb-scale tileinfo /tmp/flashdb-demo.db
```

后四行在被测量的那台机器上打印（有删节：`...` 表示省略的字段）：

```
phase=seed ok=true docs=20000 csn=40 ... write_amplification=1.207 docs_s_cumulative=29385
phase=verify ok=true csn=40 docs_a=10000 docs_b=10000 probes=40 gets=40 scans=6
phase=checkpoint ok=true csn_before=40 published=true csn=40 tiles=1 serving_memtable_entries=0
phase=tileinfo tile=tile-40.tile bytes=7033309 cutoff=40 entries=40000 page_size=4096 bloom_len=50024 dir_len=72468 fences=99 ...
```

这一段就是：一次带 WAL 的写入、一次读回校验、一次经持久 manifest 完成的 memtable→tile 发布，以及 tile 自己的格式报告。上面的每个数字都是命令的输出，不是别的什么测量。`seed` 会拒绝一个已有 WAL 的目录，所以重跑之前先删掉 `/tmp/flashdb-demo.db`。

构建命令无法重建的是 crate 本身。`--offline` 从不联网，所以缓存里必须已经有它们；在一个从未取过它们的缓存上，去掉 `--offline`，同样的固定形式会从镜像解析依赖（已用空的 `CARGO_HOME` 验证：工作区能构建）。它也既不构建 Python 测试环境（`cd harness && uv run --with pytest pytest`，需要 `uv`），也不重建那些被丢弃的测量实例与预置快照，也不做设备校准（需要 `fio` 和一块空闲的 NVMe）。

## 现在能做什么

每一条都指向钉住它的代码路径或测试。

- **存储底座：** 按提交序号排序的版本化 `P`/`U`/`R` 键（`engine/src/keys.rs`）；CRC 分帧的 WAL，尾部被撕裂时截断，遇到 CRC 损坏的帧则直接失败（fail closed）（`engine/src/wal.rs`）；按块（per-block）原子提交。
- **持久性：** 每个提交组一次 `fdatasync`，由后台线程执行，上限 `MAX_PENDING_BARRIERS = 64`（`engine/src/engine.rs:44`；`engine/tests/group_commit.rs`、`engine/tests/batch_commit.rs`）。唯一性在发布时按请求检查，而不是在 validate 阶段（`engine/tests/request_commit.rs`、`engine/tests/constraints.rs`）。
- **Tile：** `FDBTILE3` 文件格式——按页划分、带 CRC 校验的数据块（每 tile 4 KiB–64 KiB，默认 4 KiB）、一份放在磁盘上且受 CRC 保护的 fence 目录、一个稀疏的内存索引（每个*目录块*只存一条引用）、可选的 bloom 段、superblock 与 trailer（`engine/src/tile.rs`；`engine/tests/tile_format.rs`、`engine/tests/tile_layout.rs`、`engine/tests/tile_read.rs`）。
- **Manifest：** 一个链式串起续页（continuation page）的 A/B 根，使 `MAX_TILES = 4096` 个活跃 tile 放得下；一页被撕裂就退回较旧的那份有效页，链结构损坏则失败（`engine/src/manifest.rs`；`engine/tests/manifest_engine.rs`、`engine/tests/partitioned_publish_cap.rs`）。
- **维护调用：** `maybe_checkpoint(max_wal_bytes)` 发布 memtable；`rotate_wal(max_wal_bytes)` 把已完全发布的 WAL 退休到 manifest 里指名的全新段上；`maybe_compact_shape` 在三种形态之一下合并有界 runs，触发条件是每区域的 `merge_min_tiles`；`compact()` 则按调用把整个活跃列表折叠成一个 tile（`engine/src/engine.rs`、`engine/src/wal.rs`；`engine/tests/multi_tile.rs`、`engine/tests/region_compaction.rs`、`engine/tests/merge_threshold.rs`）。
- **学习轴：** 为 `P|StockMovement` 派生出的 `order_no → handle` 二级访问路径，在请求路径之外、从 memtable+tile 的合并视图构建，只当作提示使用：每次命中都被交叉校验，每次未命中都退回全扫描，只要策略要求就在每次打开时重建（`engine/src/index.rs`、`engine/src/bin/flashdb-load.rs`；`engine/tests/order_no_index.rs`）。
- **学习器可调的接口：** 每区域的 `page_size`、每区域的 `merge_min_tiles`，以及 `index_order_no` 开关，以一个 JSON 策略文件注入并按窗口重新读取（`engine/src/engine.rs` —— `LayoutPolicy`；`harness/flashdb_harness/layout_learner.py`）。策略本身不持久化：它只影响新 tile 和未来的合并，而读取方遵守每个 tile 自己存的页大小。
- **负载测试台：** 一个确定性的 inventory 生成器（SplitMix64、整数 Zipf），配字节冻结的 JSONL 夹具（`harness/flashdb_harness/generator.py`；`harness/tests/fixtures/`）；以及一个开环（open-loop）定速器，按声明速率发请求，并报告每请求的服务延迟与调度滞后（`engine/src/bin/flashdb-load.rs`）。

## 目前没有的

每一条缺失都是一个带有后果的边界，而不是一个没人需要的功能。

- **没有掉电证明。** 一个完整但 CRC 损坏的帧会被拒绝，而不是被截断，这无法区分"外部把一条已确认的写截断了"与"一条未确认的写只写了一半"；外部那轮的记录只带声明的设置（`flashdb --group-requests 64`），没有实测的持久性字段（`.scratch/82-controlled-m1/runs/b1/rung-200/run-flashdb-declared.json`；`docs/9_IMPLEMENTATION.md` 的 `PLAN-powerloss-durability` 行）。
- **没有有界的恢复时间或内存承诺。** 打开时会流式读取每个活跃 tile 并与保留的 WAL 校验，所以打开时间随数据量线性增长，校验期间的峰值由保留的 WAL 后缀决定，而不是由某个目标决定（`engine/src/engine.rs` —— `open_impl`；其注释记录了在一个 118 MB 实例上 88 MB 的打开期峰值）。
- **没有自动后台维护。** `Engine` 只起一个线程（`fdatasync` 工作线程），驱动方还可能再加一个预读告警线程（`engine/src/readahead.rs`）；它们都不做 checkpoint、rotate 或 compact。那些是打开实例的进程按自己的阈值发起的调用（`flashdb-load --checkpoint-bytes --rotate-bytes --compact-bytes`），而一个从不调用它们的进程会一直留着一个不断变大的 WAL 和零个 tile。
- **没有通用请求 API。** `engine/src/request.rs` 把 `params` 固定为 `sku/loc/qty/email/order_no`，并拒绝除 `cust → take` 以外的任何块图，所以一次请求就是一条固定的 inventory 订单，而不是一种查询语言；负载研究里的四种 join 形态有三种没有执行路径（`docs/9_IMPLEMENTATION.md` 的 `PLAN-request-surface` 行）。
- **没有多节点，没有第二个写者。** 一个进程、一个目录、一个写者。
- **布局策略不持久化。** 用不同的参数重开一个实例仍能正确服务已有 tile，只因为每个 tile 自带页大小；区域模式与策略表不是持久的（`docs/9_IMPLEMENTATION.md` 的 `PLAN-encoding-not-persisted` 行）。

## 架构

一个进程拥有一个目录。它从自己的 WAL 恢复，读一个 A/B manifest（其中指名活跃 WAL 段与活跃 tile），并通过一个只固定一种块图的请求适配层提供服务（`cust → take`，参数为 `sku/loc/qty/email/order_no`）。写入先落 memtable，由后台 `fdatasync` 线程按提交组变持久；checkpoint 把 memtable 发布成 `FDBTILE3` 格式的 tile 并链入 manifest；rotate 把已完全发布的 WAL 退休到一个新段。后台除了那道屏障（barrier）不做任何维护——checkpoint、rotate、merge 都是驱动方按自己的字节阈值发起的调用。学习轴挂在请求路径旁边：策略文件（每区域 `page_size`、`merge_min_tiles`、`index_order_no`）由驱动方按窗口重新读取并以 `LayoutPolicy` 进入引擎；派生的 `order_no → handle` 索引在请求路径之外从 memtable+tile 的合并视图构建，只作提示，并在每次打开时重建。

```
engine/               Rust crate：引擎本体，以及驱动它的三个二进制
  src/lib.rs          crate 根；Engine、请求路径、按块提交
  src/request.rs      适配层唯一接受的块图（cust → take）
  src/keys.rs         按提交序号排序的版本化 P/U/R 键
  src/wal.rs          CRC 分帧的预写日志：撕裂尾部截断，坏帧 fail closed
  src/memtable.rs     服务中的 memtable 与 memtable+tile 合并视图
  src/tile.rs         FDBTILE3：页、fence 目录、稀疏索引、bloom、superblock、trailer
  src/manifest.rs     A/B 根 manifest，续页链式串接（MAX_TILES = 4096）
  src/index.rs        派生的 order_no → handle，持久 base + 内存 delta
  src/readahead.rs    启动扫描预热与预读告警线程
  src/bin/            flashdb-scale（seed/verify/checkpoint/tileinfo）、flashdb-load、flashdb-replay
  tests/              钉住上述每条主张的集成测试
harness/              Python：确定性 inventory 生成器、开环定速器、KPI 仪器、计分
scripts/              M1/M5 的运行脚本与计分器，以及 check-docs.py
bench/                nvme-bench 与 0a-redo.sh —— 所有数字所处的设备校准
docs/                 分层的文档库：一层一个问题，按阅读顺序编号
.scratch/             测量记录——一个工单一个目录，内含 issues.md 与 run.sh
inbox/, mail/         本节点的信箱，以及它回复过的消息
```

Python 一侧从不链接引擎：`harness/` 写出字节冻结的 JSONL 轨迹和策略文件，`flashdb-load` 按声明速率开环接受该轨迹并为每个窗口写一条 JSON 记录（速率、服务与滞后分位数、字节数、索引决策），再由 `scripts/` 和 harness 的计分器把这些记录变成本文引用的表格。本文中的每个数字都能按路径追到这样一条记录；`docs/6_ARCHITECTURE.md` 拥有不变量与组件图，`docs/7_INTERFACE.md` 拥有契约，`docs/8_DETAIL.md` 拥有内部细节。

## 论点的现状

论点是：学出来的布局胜过一次声明好的布局。这里测的是两个不同的问题，只有第一个是有利的。

- **学习 vs 声明，内部，预研级（pilot grade）**（3 个种子加 1 次重复，12.25x 数据:内存实例，修订 `482e3a1`）：学习臂达到 +41.3% 到 +50.7% 的实测吞吐（中位 +47.6%），而噪声带约 4.0%；对抗性重组稳定在 0.933x 中位比值，获胜场景的重组为 0.767x；一次写尖峰损失约 8-9% 的容量，两臂都没能恢复。这不是预注册的 10 种子矩阵。记录：`.scratch/STATUS.md` §2。
- **验收，外部，输了**（`docs/2_GOAL.md` §0）：在 200 rps 下 `flashdb-declared` 维持了 5 个配对块中的 3 个，而 sqlite 维持 5/5，**postgres 18.6（Ubuntu 18.6-3build2）** 也是 5/5——最高维持水平之比为 0.5x。postgres 版本是那一臂从自己运行的服务器上读回的（`.scratch/82-controlled-m1/runs/b1/rung-200/run-postgres.json`、`postgres_settings.server_version`）；此后任何对比都要写明它测的是哪个版本，因为这台机器上的 postgres 会变，而从别的版本继承来的结论不算测量。低负载非劣性门以 1.001 的亏空通过，初始策略门也通过；3 倍优越性门因在单一提供水平下对任何一对结构上都不可达而被撤回。记录：`.scratch/82-controlled-m1/report.md` §5 与 `report.json`，修订 `51c69f3`。
- **那个 3/5 是规则无关的，不是临时读数。** 按已定的"只看滞后"规则（`sustained_primary`，`docs/2_GOAL.md` §1）重读，**45 条**已存块记录里 **0 条**、**47 条**已存 arm-run 里 **0 条**改变判定，因为没有任何 M1 窗口文件曾用服务上限计分（45 个文件里都是 `"service_ceiling_us": null`）——可用 `.scratch/87-m1-decided-rule/run.sh` 重算。所以无论如何读，这次输了都成立；反事实地打开服务上限会翻转 45 行中的 38 行，而且无法给两臂排序，因为设备自身的 fsync p99（9,821 us）就在上限之内。

这次失败被归因到一个缺陷而不是论点本身：派生的 `order_no` 索引原先只从 tile 构建，而在那个负载下 movements 还留在 memtable 里，所以它持有 0 条目，每个扫描请求都要付一次 5.6 MB 的全扫描（`docs/9_IMPLEMENTATION.md` 的 `T-83` 行；`.scratch/83-index-probe/issues.md`）。索引现在在 `8640a80` 从合并视图构建，由 `engine/tests/order_no_index.rs` 钉住，而这个缺陷已经**按每请求的工作量测量过**，而不是靠论证（`.scratch/84-index-defect-mechanism/issues.md`）：在一个按 M1 记录形态重建的追加密集实例上（156 个 tile、服务 memtable 里 2,785 条 movement、188 次扫描请求），索引持有 **2,785 条对应 2,785 行**，而修复前的构建是 **0**；**188 次扫描请求里 0 次**进入合并扫描（188 次索引查询、188 次交叉校验的点探针、同样 188 个答案）；每个扫描请求的 tile 字节从 **616,428** 降到 **0**（行还在 memtable 里时）或 **4,218**（行已落 tile 后）——按该记录的 0.047 扫描占比，每请求读字节 **42,240 → 13,466**，预测值是 43,298 → 13,460（`docs/6_ARCHITECTURE.md` §7 的 fix #1；*修复后*的数字偏差 0.05%，*修复前*的数字低 2.4%，因为重建 tile 的块填充是 3,951 B 而记录里是 4,096 B）。`engine/tests/index_defect_mechanism.rs` 钉住这一点，在修复前的代码上以 `left: 0, right: 2785` 失败。

200 rps 的对比是**未测量（unmeasured）**的。它是这个主张里唯一需要一台安静机器的部分；窗口按触发条件授予（`uptime` 的 loadavg 连续一分钟低于 2，且没有别的 agent 在写这棵树），而还没有任何配对块跑过，所以本仓库对这个修复**不声明任何吞吐数字**：站得住的是上面按每请求工作量的测量，以及下面的内部学习器结果。它不能在外部负载下重跑——`.scratch/83-index-probe/runs/` 里十二个探针文件的第一行都写着 `# INVALID - do not cite:`，原因正是这个，一次被争抢的配对测的是负载，而不是引擎。它的仪器是 `.scratch/89-m3b-paired-200/`：同一冻结轨迹上的两臂、从已提交规格重建轨迹与实例的链条、拒绝任何起始 loadavg 高于触发条件的臂的门，以及每臂一次摘要校验（digest-verified）的恢复。这道门在 2026-10-21 的 16 分钟里轮询 191 次，没有一次读数低于 2.0（最低 4.92），所以这个块在排队，这里没有任何东西是计时得来的。同类存储的借用机器仍是备选；笔记本不能替代，因为那 5.6 MB 的扫描是这台设备的性质。逐条状态见 `docs/0_ROADMAP.md` §M3 status（WP-2）。

## 开发

Rust 1.98.1，edition 2021；harness 用 Python 3.12。两个 `--config` 把 crate 源固定到 aliyun 镜像，`--offline` 禁止联网取包——这是仓库自己的运行脚本所用的封闭形式。修订 `24c94f2` 测得的套件结果：**339 passed, 0 failed, 0 ignored, 27 test binaries，无编译告警**；harness 单独测得 210 passed。`--offline` 需要热缓存（空的 `CARGO_HOME` 会在解析阶段失败，报 `no matching package named serde`）；不加那两个 `--config`，cargo 会对着 crates.io 而不是镜像解析。

```sh
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      build --workspace --offline
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      test --workspace --offline

cd harness && env -u VIRTUAL_ENV uv run --with pytest pytest -q     # 210 passed
python3 scripts/check-docs.py                     # 层名、来源分区、每一条 docs/ 引用
```

没有单独的 lint 或格式化配置：标准就是上面那套测试、编译无告警，以及 `scripts/check-docs.py` 退出码为 0。测量脚本（`scripts/m1-*.sh`、`scripts/scale-run.sh`、`scripts/m1-ladder-run.sh`）不属于这条标准——它们需要冻结轨迹、限内存的 cgroup、空闲的 NVMe，postgres 臂还需要由 `.scratch/90-m5-postgres-env/postgres-server.sh` 启动的容器；每一个都在 `docs/9_IMPLEMENTATION.md` 里连同证据记录。每条测量记录都写明它起始时的 loadavg，而在外部负载下取得的计时结论是无效而不是近似——`.scratch/83-index-probe/runs/` 里十二个标着 `# INVALID - do not cite:` 的文件，就是忽视这条规则的代价。

## 文档

文档按层归档：编号越小，文件越靠近*为什么*而不是*怎么做*，每个文件只拥有一层。按这个顺序读。

| 文件 | 作用 |
|---|---|
| `docs/1_PROBLEM.md` | 为什么这件事值得做：论点，以及已有工作已覆盖的部分。 |
| `docs/2_GOAL.md` | 标准线：外部对比、KPI 阶梯、内部目标，以及 v0 的范围内外。 |
| `docs/5_PRODUCT.md` | 学习器：它控制什么，一次提升如何被出资与证明，以及仍未决定的分叉。 |
| `docs/6_ARCHITECTURE.md` | 不变量、组件图，以及被归因到某个阶段的实测外部差距。 |
| `docs/7_INTERFACE.md` | 四份契约：逻辑模型、事务模型、持久性与失败、布局。 |
| `docs/8_DETAIL.md` | 引擎内部：键空间、提交、tile、压缩、manifest、恢复。 |
| `docs/9_IMPLEMENTATION.md` | 负载与 harness、构建顺序、主张→产物的证据，以及未决项清单。 |
| `docs/0_ROADMAP.md` | 里程碑、门、带负责人与验收标准的工作包，以及生效中的决定。 |
| `docs/0_STAGING.md` | 事实从到达到归档之间的位置；一旦有了归宿就不停留在这里。 |
| `docs/glossary.md` | 术语表，每条都指名拥有其含义的层文件。 |

`scripts/check-docs.py` 用一条命令判定四项结构性质：层文件名、每个文件内的来源分区（source-tier）、每条 `docs/` 引用是否可解析，以及文档库在迁移中没有丢行。

参考目标：一块 NVMe，数据:内存 ≥20:1，在受 cgroup 限制的实验里（例如 200 GB / 8 GB）。

## 路线图

**已实现** —— 引擎本身，以及目前用来裁决论点的一切：

- 日志结构底座：能处理撕裂尾部的 WAL、A/B manifest、`FDBTILE3` tile 与按块原子提交；组提交每组一次 `fdatasync`。
- 学习轴：每区域可调的策略面（页大小、合并阈值、派生索引开关）按窗口重载，以及从 memtable+tile 合并视图构建、持久 base + 内存 delta 的 `order_no` 索引。
- 预研级的内部学习器结果：T1（吞吐）与 T3（漂移恢复）成立，T2（重组成本）成立，T4（写尖峰恢复）**不**成立。
- 外部基线记分板已测且输了；差距按工单归因到具体缺陷，修复按每请求工作量测量而非论证。
- 负载测试台（带字节冻结夹具的确定性生成器、开环定速器、KPI 仪器、计分）、按层归档的文档库，以及作为其常设检查的 `scripts/check-docs.py`。

**计划中** —— 按依赖顺序，来自 `docs/0_ROADMAP.md`：

- **M5 —— 公平的三臂重跑。** 论点里唯一需要安静机器的部分：在同一冻结轨迹上对着 sqlite 与 postgres 打 200 rps，所有效率仪器接上。它是"被挂起"而不是"空转"：仪器、重建链条，以及拒绝超过触发条件的臂的 loadavg 门都已写好（`.scratch/89-m3b-paired-200/`）；postgres 腿现在从一个受检查的容器里跑，并回报它测到的版本；在任何一对落地之前不允许声明吞吐数字。
- **M6 —— 预注册的 10 种子裁决与发布。** 在 ≥10x 实例上以 10 个独立种子跑 T1-T4，阈值完全按预注册，独立复核后，无论结果如何都发表。
- 其间可以在负载下完成的无机器工作：约 573 ms 屏障组没有替代品的按组阶段计时器，以及 WAL 预分配的结论（代码部分已写完并单元测试，计时未测）。
- 明确**不做**：通用 L2 共置（co-location）、多节点运行，以及比已提交块图更宽的查询语言——每一条都是 `docs/0_ROADMAP.md` 里的决定，而不是待办事项。

接下来往哪走：有意思的不是"学出来的布局能够被造出来"，而是它能否在别人也买得到的硬件上被证明更优。仓库的摆法让一个否定答案也可以发表——这次失败已经被记录、被归因、也被部分修复——而剩下的那个数字，等机器安静下来，是从记录里读出来的，不是被断言的。

## 参与贡献

见 [CONTRIBUTING.md](CONTRIBUTING.md)：范围、必须通过的检查，以及一个 pull request 应当带上什么。问题请开在 <https://github.com/ZisIsNotZis/flashdb/issues>。

## 许可

GNU Affero General Public License v3 —— 见 [LICENSE.md](LICENSE.md)。
