# 存储与标签查询运维

P0–P2 的实现与验收结果见[优化方案](storage-query-optimization-plan.md#14-实施结果与阶段决定)。本次没有连接或清理生产数据库。008–012 为新增迁移；原始 archive、完整导出 schema v1、请求和 session 身份不变。新库默认 `DualWrite`；旧消息会继续通过兼容投影读取，**不会自动清空旧正文**。

## 配置与连接预算

`llmtrace.example.toml` 的 `[note_index]` 提供完整示例。默认开启一个 worker，独立 2 连接池；管理查询另用独立 4 连接池和进程级 8 个执行名额。它们都在原 `storage.max_connections` 之外：默认单实例最大为 10 + 2 + 4 个应用连接，另预留运维、备份和其他实例连接。

| 预算 | 默认值 / 行为 |
| --- | --- |
| 新请求领取 | 空闲时每秒轮询；任务完成后立即补领，不限制为每秒一个请求 |
| 历史回填 | 每 30 秒最多入队 100 个请求；优先级低于新请求 |
| 请求 / 解压 segment | 8 MiB / 64 MiB；超限记 `partial/body_budget`，不会无限分配 |
| 扫描 / 字段 | 8 MiB / 8192 个文本字段 |
| 标签 | 单值 4096 字节、最多 256 个、总值 256 KiB；不截短后当作完整索引 |
| lease / 尝试次数 | 60 秒 / 3 次；过期旧 token 不能发布，失联重领也计入次数 |
| 关闭 | 停止领取、协作停止解析，10 秒后若仍在运行返回拥有任务句柄的状态；main 继续显式 join 后才结束 |
| 查询 | 连接等待 500 ms、SQL 4 秒、总期限 6 秒、响应 8 MiB、每页最多 500 行 |

正文读取先检查压缩与解压大小。历史 request/response 共用一个 segment 时仍需解压该 segment，但不为标签加载另一个独立 response segment。请求完整而响应截断或损坏，不会因此降低请求标签的完整性。

`note_index.enabled = false` 停止派生 worker；采集事务仍记录轻量 pending。暂停历史回填不影响新请求索引。预算配置非法会拒绝启动，不静默修正。当前仅实现提取版本 1；未知版本明确失败。

## 迁移前测量

所有维护命令要求显式 `--config`（也可由 `LLMTRACE_CONFIG` 提供）。先确认配置和 `DATABASE_URL` 指向计划维护的数据库。示例命令中的 `./llmtrace` 为已构建的二进制：

```sh
./llmtrace --config llmtrace.toml --check-config
./llmtrace --config llmtrace.toml --migrate-only
./llmtrace --config llmtrace.toml --message-maintenance measure
./llmtrace --config llmtrace.toml --message-maintenance status
```

`measure` 是显式离线聚合：按精确 UTF-8 文本统计预览总字节、去重后字节、出现数、短正文数；同时报告请求大小与时间范围、请求种类、session 长度、角色、标签数量/覆盖、关系 heap/index/TOAST 辅助占用及 dead tuple 估计。逻辑统计使用 repeatable-read；物理与统计视图是观测值。`wal_server_global` 是服务器累计 WAL，带 reset 时间，**不是此数据库或本条命令的 WAL 增量**。单条 SQL 超时 30 秒；大库应在低峰执行，超时意味着测量未完成。

结合 `/api/storage/summary`、现有 archive/runtime 指标和宿主机监控，单列 archive、journal、备份、孤儿文件与可用空间。不要把逻辑字节节省当作磁盘已经释放。容量计划至少包含旧正文、新字典、引用索引、WAL/备份和可能重写的临时空间。报告中的无重复样本增长说明不能假设所有流量都值得去重。

## P1 分批切换

1. 部署支持兼容视图和新写入模式的全部实例。升级前后的写入混跑只能停留在兼容阶段；数据库无法替操作者证明旧进程都已退出。
2. 在有数据的旧库上显式准备在线索引和约束，再回填。空库的相同索引已经随迁移创建；`prepare` 可重复执行并修复本功能留下的无效索引。

```sh
./llmtrace --config llmtrace.toml --message-maintenance prepare
./llmtrace --config llmtrace.toml --message-maintenance dual-write
```

3. 为这次部署设置数据库停止阈值，并在**每批之前**检查数据库卷、WAL 卷和备份余量。下面的 50 GiB 只演示参数格式，必须换成该环境预留余量后的阈值。

```sh
./llmtrace --config llmtrace.toml --message-maintenance backfill \
  --message-batch-size 1000 --max-database-bytes 53687091200
```

一次命令只提交一批（1–10000 行）。操作者或已有运维调度器控制批次间隔；停止调用就是暂停，再次调用即可继续。若空间低、WAL/复制延迟升高、采集积压增长或连接竞争，先停止回填并缩小批次/降低频率。数据库大小参数只在分配批次前检查，不代替磁盘/WAL 监控，也不能预测本批峰值。命令采用 2 秒锁等待，失败回滚并记录稳定的 `batch_failed`，不持久化正文或 SQL 参数。

回填每次重新扫描缺失引用，记录高水位与累计行数，不用高水位跳过晚提交的低 ID。重复调用直到 `affected_rows = 0`，然后运行 `status`，要求缺失引用、悬空引用和双写不一致均为 0。`status` 是显式精确验证并更新快照；管理完整性 API 读取带检查时间的快照，不声称是当前实时精确数。

4. 对同一批 session 验证消息 ID/顺序/角色/三态截断、分页、原始字节和完整导出 end marker。使用代表性流量确认空间净节省和吞吐/读延迟门槛（方案第 13 节）。确认全部 writer 已升级后，才提交操作者的门槛确认：

```sh
./llmtrace --config llmtrace.toml --message-maintenance references-only \
  --acknowledge-migration-gates
```

该确认仅代表操作者已经完成部署与性能验证。命令仍会独立验证回填、文本、索引和约束。没有确认或数据验证失败均拒绝切换。

5. 按批清空旧列；每批再次比较文本和 UTF-8 字节长度，不会仅凭 ID 清空。

```sh
./llmtrace --config llmtrace.toml --message-maintenance clear-inline --message-batch-size 1000
./llmtrace --config llmtrace.toml --message-maintenance status
```

清理完成后按既有维护策略执行 `VACUUM (ANALYZE)`。普通 VACUUM 主要回收表内可复用空间；`VACUUM FULL`/表重写需要维护窗口、强锁和临时磁盘余量，应单独安排。字典延迟 GC 随 archive 维护执行；也可显式调用 `--message-maintenance gc --message-batch-size 1000`。GC 锁定候选后重查引用，不删除仍被任何请求引用的正文。

## P1 回退

清空正文之后，必须先恢复 inline 才能回滚到旧二进制。保留可写的新版本并准备恢复所需空间：

```sh
./llmtrace --config llmtrace.toml --message-maintenance dual-write
./llmtrace --config llmtrace.toml --message-maintenance restore-inline \
  --message-batch-size 1000 --max-database-bytes 53687091200
./llmtrace --config llmtrace.toml --message-maintenance status
```

重复恢复到 `affected_rows = 0`，确认 `missing_inline = 0`、无文本不一致，再运行 `--message-maintenance legacy`。需要撤销去重收益验证时，Legacy 下分批运行 `discard-references`，最后 `gc`。引用和字典可保留为派生数据；不要删约束、视图或 schema migration 记录来绕过检查。

还有一项启动兼容要求：基线 `af2d800` 使用 SQLx 默认 migration 校验，原发布二进制会拒绝数据库中它不认识的 008–012。回退可先保留当前二进制的 Legacy 模式并关闭标签 worker。若要回退业务代码，须准备**携带本次相同 008–012 migration 文件的旧代码兼容构建**，在隔离库验证启动和读写后再使用；仅恢复正文还不能让未经适配的旧二进制通过 schema 检查。不要删除 `_sqlx_migrations` 的新记录伪装成旧库。

若尚未清空旧列，也必须确认全部新写入仍有 inline；不能仅依据“没运行清理命令”推断 ReferencesOnly 期间不存在 NULL。

## P2 索引控制与恢复

在 **Admin → System → Prompt tags** 配置大小写敏感的标签名。初始为空，保存后新请求使用新配置，保留的历史请求在回填开启时分批重建。更改配置后旧命中和旧游标立即失效。清空列表会关闭标签提取。012 迁移保留原始请求，旧的派生索引在首次配置后重建。

```sh
./llmtrace --config llmtrace.toml --note-maintenance status --note-version 1
./llmtrace --config llmtrace.toml --note-maintenance pause-backfill
./llmtrace --config llmtrace.toml --note-maintenance resume-backfill
./llmtrace --config llmtrace.toml --note-maintenance backfill --note-version 1 --note-batch-size 100
./llmtrace --config llmtrace.toml --note-maintenance retry-failed --note-version 1 --note-batch-size 100
```

历史回填开关在下一个回填周期生效，已入队任务继续处理。显式 `backfill` 只入队一个有界批次，不读取原始大正文；重复调用不会跳过晚提交数据。失败先检查归档是否仍存在、连接和预算，再有限重试；来源已删除不重建请求。Partial/失败重试保留已发布的正命中，Ready 发布才原子替换该请求/版本的出现记录。

失联 lease 也消耗尝试次数；耗尽的任务转为 failed，领取流程继续寻找其他可处理任务。大量旧失败任务不会被误报成整个队列暂无工作。预算配置在启动和提取器边界校验，零值不表示无限制。

未配置标签时请求状态为 `disabled`。启用后状态为 `pending / processing / ready / partial / unsupported / failed`，请求详情可按需读取 `/api/requests/{id}/prompt-index`。`missing_source` 是诊断计数，与 failed 等状态可重叠，不能作为互斥状态相加。`external_history` 表示 provider 侧历史未捕获；即使本次捕获 ready，也不能推出远端未捕获历史不存在标签。

未来增加受支持提取版本后，可将 worker 的 `build_version` 指向该版本，历史回填与旧版本查询并行；默认省略该项则跟随 active_version。`--note-maintenance activate --note-version N` 只接受本二进制已支持的版本，且拒绝仍有 missing/pending/processing/failed 的切换。Partial/unsupported 仍保留为未知覆盖。切换使用数据库锁与入库 pending 创建协调；旧 cursor 失效后从第一页重新搜索。当前版本不能用 `N=2` 提前创建未知语义。

可选子串加速（需要安装扩展/建索引权限）：

```sh
./llmtrace --config llmtrace.toml --note-maintenance prepare-substring
```

该命令在事务外创建 pg_trgm 与并发 GIN 索引，重复执行安全并修复无效索引。精确搜索不依赖扩展；子串查询没有扩展也可运行，但受统一超时约束。短串、符号串和高命中可能没有有效 trigram，需按自己的数据测量；UI 不承诺所有 substring 都被索引加速。

## 观察与查询结果解释

监控 `llmtrace_note_index_running`、`llmtrace_note_index_published_total`、`llmtrace_note_index_failures_total`，以及原有 trace/journal backlog、drops、build/persist failures。索引停止不等于原始审计停止；readyz 继续表示服务原有的就绪条件。

标签搜索返回当前保留请求中已索引的正命中；覆盖信息始终保守为 `unknown`，计数来自每 30 秒刷新的全保留集维护快照，`coverage_counts_exact = false`，并非每个过滤窗口的精确计数。零命中不能证明不存在。状态命令可刷新快照，但不会使未捕获的外部历史可搜索。

查询发生 429/503/504 或更改条件时，UI 保留并标明上次结果，拒绝旧请求覆盖新结果。取消会中断前端请求；服务端 SQL 仍有独立 4 秒上界，连接/permit 的释放有数据库锁屏障测试。JSONL query export 仍是有限行的当前查询，不能代替完整 session 导出。详情见[查询编辑器](query-editor.md)。
