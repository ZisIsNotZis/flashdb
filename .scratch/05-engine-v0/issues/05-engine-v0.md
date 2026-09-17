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

## Acceptance criteria

1. 实验 0a 数据可复现（fio 命令 + JSON 在 evidence/）。
2. 键空间编码有属性测试（排序不变量、roundtrip、非法名拒绝）。
3. `cargo test` 全绿；harness 生成器可产出确定性 trace（同 seed 同字节）。

## Not in scope

多节点、在线学习、canary、SQL。
