# 0a-redo：fio 与 Rust O_DIRECT 同窗口重测（2026-09-28 10:34–10:36）

判定 reduced-load：运行期 133 个每秒样本，最低 idle 57%，平均 66.6%，max load1 4.54。8 份 fio JSON 均由本次运行新生成；`summary.json` 为本次摘要。前一次 v7 因 fio 缺失读到旧 JSON，其跨工具比较**无效**。

| 测试 | fio | Rust `O_DIRECT read_at` | 注释 |
|---|---:|---:|---|
| 4K QD1 均值 | 96.3 µs | 96.3 µs | 同窗口相等；无证据支持 40–60 µs syscall 税 |
| 4K QD32 IOPS | 202,924 | 203,658 | 相差约 0.36%；两种路径均接近设备饱和 |
| 4K QD64 IOPS | 205,095 | 205,886 | 约 205k IOPS 饱和 |
| 16K QD1 均值 | 117.4 µs | 未测 | 16K 成本只能引用 fio，不能代入 Rust 的 4K 结果 |
| 64K QD1 均值 | 185.8 µs | 未测 | reduced-load |
| 顺序 1M | 831 MiB/s (fio QD8) | 737 MiB/s (Rust QD1) | 队列深度不同，不可归因于 syscall |

## 对旧结论的撤销

2026-09-28 09:15 的 Rust 4K QD1 单次测到 149 µs、当时 fio JSON 却仍是 09-19 的旧数据；把两者相减并解释为“Rust syscall 开销”是错误的。把该 **4K** 数字称作“16K 依赖页读”同样错误。本次同窗口的 4K 均值相等，说明上次差异更可能受时间/负载影响，但单独归因仍不能成立。旧 09:15 Rust 原始输出已被本次运行覆盖；修正记录留在这里。

当前只能把 16K QD1 ≈117 µs、4K QD32 ≈203k IOPS、fio 1M QD8 ≈831 MiB/s 作为 reduced-load **参考**。最终洁净窗口常量仍需严格空闲重测。Rust 基准的 `seq` 是 QD1，fio 是 QD8；不能直接比较带宽路径成本。
