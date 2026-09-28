# 05-engine-v0 — v0 引擎实现与实验

Status: claimed
Need-review: yes（每个有行为的里程碑）

## 语言决定（作者委托："实现层我真的没什么想法"）

- **引擎 = Rust 1.98**（rustup stable）。理由：内存安全是作者的显式需求（"memory safe, robust, must not crash itself"），性能是论文本体，单文件部署、无 GC 停顿。
- **harness / 实验 = Python 3.12 + uv**。理由：数据科学生态、迭代快，与引擎通过 trace 格式解耦。

## 已完成

### 实验 0a — 设备微基准（fan-out 前提实测）

见 `evidence/analysis.md` + `evidence/fio-*.json`。设备：CN600 2TB NVMe（裸分区，无加密），8GB 预分配文件，O_DIRECT，fio 3.36 / libaio。

- 批量并行 = **9.3×**（10.4k → 96.7k IOPS）：独立扇出必须并发下发。
- **"塌缩成整瓦片读"被证伪**：8 MB 顺序读 ≈ 10 ms > 20 个串行 4K（1.9 ms）。页粒度 + 反向索引 + 批量并发的引擎设计被实测确认。
- 页大小默认 64K → **16K**（`docs/engine.md` 已按校准更新）。
- 设备尾 p99 ≈ 10× 均值：SLO 分析用 percentile。

## 下一步（顺序）

1. harness：trace 格式 + workload 生成器（inventory + heavy-join）+ replay。
2. 实验 0b：离线 layout 搜索 vs declared-layout 控制。
3. 引擎骨架：keys（已完成）→ WAL + memtable + `P/` 点读 + 按块提交。

## 勘误（2026-09-17）：实验 0a 数据污染

作者告知测量期间机器在跑渲染负载。`evidence/analysis.md` 已标记污染：
绝对常量作废，"设备尾 p99 ≈ 10× 均值"结论最可疑（渲染争用即典型尾延迟来源），
相对方向降级为假设。`docs/engine.md` 的 16 KB 页默认改为 provisional。

**方法论守卫（永久）：基准脚本必须记录运行期间的 `/proc/loadavg` 与 iostat 采样，
负载超阈值则结果标记 contaminated，不进校准。空闲后重跑 experiment 0a-redo。**

## Addendum: 实验 0a-redo 完成（2026-09-28）

2026-09-28 09:15 的 v7 运行里 fio 缺失，脚本误用 09-19 旧 JSON 并退出 0；旧“双工具交叉验证”及“149 µs 为 16K 真成本 / 差额是 syscall 税”的结论**撤销**。脚本现要求 fio 存在，清除旧结果，任何 fio 失败即非零退出。

2026-09-28 10:34–10:36 同窗口重新运行：8 个 fio JSON 新生成；reduced-load（133 个采样，最低 idle 57%）；4K QD1 fio/Rust 同为 **96.3 µs**，4K QD32 **202,924/203,658 IOPS**（差约 0.36%）；16K QD1 fio **117.4 µs**，Rust 未测 16K；fio 1M QD8 顺序 **831 MiB/s**。仅供 reduced-load 参考，不冒称 clean-idle。分析：`evidence/0a-redo/nvme-bench-analysis.md`。

## 2026-09-28 — 唯一索引发布检查缺陷（Agent）

发现并复现：`U` 前缀按 handle 再按 CSN 排序；原先只读此前缀第一项，低 handle 墓碑会遮蔽较高 handle 的活索引，导致第三个文档可占同一 `x-unique` 值。新增回归测试先失败；修复为按 handle 各取最新版本，再合并块的终态 overlay 检查活 owner 数 ≤1；同块释放并转移允许。所有 Op 的名称在 WAL 写入前验证，避免写入成功后 `unwrap` 崩溃。`cargo test -p flashdb-engine -q` 30/30；`git diff --check -- engine` 通过。Need-review: yes；独立验证工具入参契约矛盾（`provided` 同时要求和禁止 `reason`），暂以新旧行为对照与 scoped self-review 留证，后续复核。

## 2026-09-28 — 唯一字段读取辅助（Agent）

新增 `Engine::unique_lookup`：按 handle 各取快照内最新版本，检查索引 value 与句柄一致；多个活 owner 返回损坏错误，不任意选择。测试覆盖删除、同块转移、历史快照、WAL 恢复，`cargo test -p flashdb-engine -q` 31/31，通过 `git diff --check`。此辅助是请求翻译层唯一 probe 的前置依赖。

## 2026-09-28 — 库存请求适配器（worker → Agent 集成中）

隔离工作树实现 `engine/src/request.rs`，仅支持 harness `order_flow` 的精确 `cust → take` 模板，不声称通用语法。`cf2cd99` 集成后 `cargo test --workspace --offline` 为 31 个单元 + 4 个集成测试全绿；端到端涵盖库存成功扣减并同时写 movement/order、欠库存回退 backorder、反向引用、WAL 恢复。Agent 增加同一请求 ID 不同 payload 的回归：原实现错误确认旧提交，正在修复（本票据后续记录最终 revision）。

限制：读取绑定尚未随 WAL 记载，读块重放不可返回原观察；没有通用读集冲突验证、TTL 幂等保留、通用请求规范化/任意 DAG。使用者不可将此原型当成完整事务接口。独立审查超时，但尾部结果标记三个 P1：不同 payload 错误重放（已修，回归测试）、唯一冲突误写欠单（此窄适配器以 `WouldBlock` 拒绝，不作为缺货 fallback；通用合同对冲突可触发 else 的表述仍须设计澄清）、过长 WAL 记录提交后恢复丢失（append 写前按 MAX_RECORD 拒绝，回归测试）。保留审查输出在子任务记录；不要把超时当作无发现。

## 2026-09-28 — 独立复审修正（Agent）

Fresh reviewer 针对 `761ee4c..2a8f6c4`（审查材料 741 行）确认先前三项 P1 已修，又发现跨实体同一 `order_no` 可先有 Order 后有 Backorder（反向亦然）；原有 U 前缀只在各实体内独立。适配器现于单写者 `&mut Engine` 边界检查两实体 `order_no`，任一已存在则返回 `AlreadyExists`，不发布另一状态。回归测试覆盖两种顺序与 reopen；`cargo test --workspace --offline -q` 32 单元 + 6 集成通过，`git diff --check` 通过。独立复审此前 verdict BLOCK 对旧 diff 有效；此项修复经 scoped self-review 和回归验证，最终 revision 待下方提交记录。仍待通用契约决定冲突是否进入 `else`；此窄适配器只在库存条件失败时生成欠单。

## 2026-09-28 — 作者确认条件与异常边界（Agent）

作者指出 `else` 对应 `if`：只有预期中可作为条件的失败才走回退；业务唯一性冲突是原则性异常，直接失败；仅并发时机造成的锁/乐观验证竞争可能重试。实现将当前唯一性 `Outcome::Conflict { kind: "x_unique" }` 映射 `AlreadyExists`，不再错误地返回表示时机竞争的 `WouldBlock`；未来并发验证失败另设结果类型。测试覆盖发生唯一冲突时不写欠单。通用规则已写回 `docs/contracts.md`，引擎设计 `docs/engine.md` 同步；当前仍无并发读集验证，不声称已支持自动重试。`cargo test --workspace --offline -q` 32 单元 + 6 集成通过，`git diff --check` 通过。

## Acceptance criteria

1. 实验 0a 数据可复现（fio 命令 + JSON 在 evidence/）——**需在空闲机器上重跑（0a-redo），且带负载守卫**。
2. 键空间编码有属性测试（排序不变量、roundtrip、非法名拒绝）。
3. `cargo test` 全绿；harness 生成器可产出确定性 trace（同 seed 同字节）。

## Not in scope

多节点、在线学习、canary、SQL。
