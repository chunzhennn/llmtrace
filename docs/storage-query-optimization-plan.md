# llmtrace 存储与查询优化修改方案

状态：P0–P2 已实现并完成本地验收；P3/P4 保留准入门槛。基线：`af2d800`。首次验收：2026-11-16；首版复核：2026-11-17；可配置标签复核：2026-11-18。

迁移、回填与回退步骤见[运维说明](storage-query-operations.md)。第 14 节记录实施结果及需要在部署环境确认的门槛。

本方案优先减少消息正文的重复存储，并让用户能从管理 UI 可靠地查询输入 prompt 命中已配置标签的 session。原始捕获、请求计数、会话回放、统计、消息预览和完整导出保持兼容。实现分阶段交付，不把消息去重、查询引擎重写、CDC 归档和对象存储合并成一次改造。

**本次完成 P0、P1、P2：测量、消息预览去重、标签查询闭环。P3 无损分块归档和 P4 冷热迁移继续分别按准入门槛决定，不进入生产存储路径。**

P0–P2 的新表、配置、API 字段和模块已落地。P3/P4 仍是条件设计；离线 CDC 原型只用于测量，不代表已有 manifest、pack GC 或远端归档能力。容量背景使用用户提供的汇总；本次验收只使用隔离合成数据，没有推断生产数据库的重复率或承诺压缩倍数。

审阅导航：第 3 节评估复杂度；第 5 节说明兼容迁移与回滚；第 6 节定义协议和标签边界；第 7–8 节覆盖查询及 UI；第 11 节规定 Rust 实现边界、类型和任务生命周期；第 12–13 节列出故障场景和验收标准。

## 1. 目标与不变量

### 1.1 必须保持的行为

| 行为 | 优化后的约束 |
| --- | --- |
| HTTP、SSE、WebSocket 转发 | 不新增同步解析、磁盘、压缩或数据库等待；保持原有路由、上游、超时与捕获限制 |
| 录制可靠性 | 保留 bounded `try_send`、内存预算、journal、幂等 receipt；不宣称 HTTP 成功即审计持久化 |
| 请求和统计 | 每次请求仍是独立事实；相同正文的重试仍分别计数、计费和记录错误 |
| 原始正文 | 保留期内可恢复原有捕获字节；长度、SHA-256、截断标记和编码语义一致 |
| 消息预览 | 保留现有字段、ID、行数、顺序、role、文本及三态 `content_truncated`；不进一步缩短内容 |
| 会话身份 | 沿用当前上游、凭据和显式 hint 的分组；正文相同不合并 session |
| 管理查询 | `/api/query` 继续使用白名单、绑定参数和受限语法；不开放 raw SQL |
| 详情和导出 | metadata-only 读取不加载 body；完整导出的记录格式、计数和 end marker 保持兼容 |
| Transcript | 保留 Worker、背压、分支和输出显示规则；存储去重不能影响展示层的上下文匹配 |
| 历史数据 | 原有 inline body、归档 segment、文件系统和 PostgreSQL blob 继续可读 |

“完整”始终指已捕获且仍在保留期内的数据；不包括未捕获的 provider 历史、二进制 WebSocket 原文或已经轮转删除的数据。

### 1.2 首轮不做的工作

- 不增加 Elasticsearch、向量库、Redis 或 Kafka。
- 不新增逐请求保存完整 prompt 的第三份大字段。
- 不根据解析后的消息重新序列化原始 JSON，也不只保存“本轮新增消息”。
- 不将标签查询扩大成通用 JOIN、任意表达式、任意正则或递归条件引擎。
- 不改变计费、token、工具调用、TTFT/TTFB 的现有解析口径。
- 不通过缩短正文保留期来兑现空间收益。
- 不顺带重构整个 proxy、auth 或存储 facade。

## 2. 当前状态与证据

### 2.1 容量背景

用户提供的 llmtrace 汇总约为：压缩归档 16 GiB、数据库 17 GB、118,989 个请求、约 4 天 5 小时；其中 `session_messages` 表及其相关存储约 16 GiB。该分布提示消息重复和归档轮转值得优先处理，但不能直接推出唯一消息量、去重后物理大小或 30 天最终容量。

测量时统一 GiB/GB、物理/逻辑大小及时间窗口。分别记录 heap、TOAST、索引、dead tuples、WAL、归档、journal、备份和孤儿文件，避免将 `sum(length(content))` 与数据库物理体积混为一谈。PostgreSQL 已可能对大字段做 TOAST 压缩；内容去重的收益应在此基础上计算。

### 2.2 基线代码行为（af2d800）

| 源码 | 基线行为及影响 |
| --- | --- |
| [storage/ingest.rs](../crates/llmtrace/src/storage/ingest.rs) | 每个请求将解析预览完整插入 `session_messages`，相同历史文本可重复出现 |
| [trace.rs](../crates/llmtrace/src/trace.rs) | 预览每条最多 16 KiB、每请求最多 128 条，可能丢弃中间消息；也会去除 NUL 并标记变化 |
| [storage/archive.rs](../crates/llmtrace/src/storage/archive.rs) | 一次仅准备一对 request/response，随后封存 segment；增大 segment 阈值不会自动实现跨请求压缩 |
| [storage/bodies.rs](../crates/llmtrace/src/storage/bodies.rs) | 详情和导出共享解码、完整性检查与历史兼容逻辑，应继续作为唯一 body 读取入口 |
| [storage/query.rs](../crates/llmtrace/src/storage/query.rs) | 四个白名单 dataset、AND 过滤、最多 500 行；尚无会话关联内容查询或统一 cursor |
| [storage/sessions.rs](../crates/llmtrace/src/storage/sessions.rs) | 会话详情直接读取 `session_messages.content`，去重后必须迁移到兼容投影 |
| [storage/session_export.rs](../crates/llmtrace/src/storage/session_export.rs) | 完整导出读取原始归档及预览，使用 repeatable-read、流式发送和最终计数校验 |
| [storage/rotation.rs](../crates/llmtrace/src/storage/rotation.rs) | 容量轮转删除最旧请求，不只是驱逐本地副本；目前不限制整个数据库大小 |
| [storage/retention.rs](../crates/llmtrace/src/storage/retention.rs) | 空会话另行清理；删除请求并非直接级联删除所属 session |
| [transcript.md](transcript.md) | 完整 transcript 读取原始导出；前端上下文折叠不等于物理存储去重 |

### 2.3 需要同步处理的查询正确性问题

以下问题已在本次实现修复并补回归测试：

1. 完整插件路径直接用作 SQL 别名，超过 PostgreSQL 标识符长度后可能截断碰撞。改成短内部别名再映射 JSON 键，不能限制用户本来合法的完整路径作为唯一修复。
2. Builder 整数转换未检查安全整数，而 Code 模式会检查；`9007199254740993` 可被 Builder 改成 `9007199254740992`。两种模式及嵌套 JSON 必须统一校验。
3. 标签包含过滤使用 `= ANY(tags)`，与现有 GIN 索引不匹配。评估改成数组包含操作符并覆盖空数组、NULL 元素等边界。
4. 结构化查询只有 limit，没有 `has_more`；默认时间排序缺少唯一键。新增查询能力必须明确结果是否还有更多。

## 3. 阶段划分与复杂度控制

| 阶段 | 内容 | 新增复杂度 | 进入下一阶段的条件 |
| --- | --- | --- | --- |
| P0 | 基线、协议 fixtures、兼容性和收益测量 | 低，离线工具和测试 | 明确数据重复率、查询基线、资源预算 |
| P1 | 预览正文字典、兼容读取、回填与回收 | 中，一张正文表和小范围存储修改 | 去重收益成立，原查询/导出一致，写入无明显退化 |
| P2 | 按请求协议提取的标签索引、sessions 关联过滤、UI 闭环 | 中，受限提取器和一个可恢复索引 worker | 覆盖率可解释，Builder/Code 等价，真实规模查询达标 |
| P3 | 原始字节 CDC、manifest、不可变 pack、GC | 高，新的归档版本 | 在独立基准中证明净收益足以抵偿索引、GC、读放大和维护成本 |
| P4 | NAS/对象存储、本地缓存、分离保留策略 | 高，远端可用性、缓存和生命周期 | 本地存储确实成为瓶颈，P3/现有归档均有可靠读取和迁移协议 |

P1、P2 不依赖 P3、P4。若消息字典已经满足容量目标，可以停在 P2。P3 无法通过收益或可靠性门槛时保留当前归档，不为了统一架构强行切换。

首轮只保留三种表述明确的数据：请求事实、去重后的预览内容、从请求中提取的标签。原始归档仍是权威来源。预览字典与原始字节存在派生重复，不能声称已经实现“所有正文全系统只存一次”。

Rust 实现按应用代码设计：使用具体类型、普通函数和现有 SQLx/Tokio 设施，不增加协议插件注册框架、通用 repository trait、泛型存储引擎或为未来后端预建的动态分发层。P1/P2 不需要增加 workspace crate。模块按纯提取、数据库操作和任务调度的真实边界划分；类型可在同一叶模块中定义，不强行每个小类型一个文件。

## 4. P0 测量与设计准入

### 4.1 必须采集的指标

- 请求数、协议、大小、会话长度、角色、时间分布；至少覆盖连续会话而不只是随机请求。
- 预览总字节、按精确内容去重后的字节、出现记录数、不同正文数、短文本占比。
- 消息字典的预计行与索引成本，特别是内容几乎不重复的流量。
- 完整归档样本的现有 zstd 大小、zstd 1/3、不同 CDC 块大小的净大小和恢复耗时。
- PostgreSQL 查询 p50/p95/p99、buffer reads、排序溢出、连接等待、入库吞吐、journal 积压和丢弃计数。
- 标签的每请求数量、不同值数量、长值、未知协议、截断捕获和 provider 外部历史比例。

去重测量按当前预览的精确持久化文本进行；raw body 分块另按原始字节进行。不能对规范化后的 JSON 估算原始字节去重收益。

### 4.2 数据集与准入决定

使用合成数据和受控脱敏样本；不把真实 prompt、凭据或捕获文件提交进仓库。合成集包含低/高重复率、长 session、分支、重复重试、并发入库、无命中及高命中搜索。

性能报告必须同时列出绝对占用和压缩比。参数选择先比较少量候选，不引入运行时自适应调参系统。压缩等级 3 只是候选，并不预设优于当前等级的综合吞吐。

## 5. P1 消息正文去重

### 5.1 最小数据模型

不另建一套 `message_occurrences` 表；复用现有 `session_messages` 作为出现记录，保留原有外键、ID、序列和时间索引。

已新增：

```text
message_contents
  id                  bigint primary key
  content_sha256      bytea unique, 固定 32 字节
  content_bytes       integer
  content             text not null

session_messages
  原有字段保持
  content_id          bigint nullable -> message_contents.id
  content             text nullable，迁移期/回滚保留
```

当前一个 llmtrace 数据库作为一个去重域，不新增租户管理体系。若以后多个授权隔离域共享表，须先把隔离键加入唯一约束及所有查询；不能通过全局正文 hash 的 API 暴露其他域内容。

约束为 `content IS NOT NULL OR content_id IS NOT NULL`；空字符串合法，不等于 NULL。外键禁止级联删除出现记录。过渡期两者都有值时，必须校验内容相同。

正文身份仅包含精确 UTF-8 文本，不包含 role、session、时间或截断状态。`content_truncated = NULL/false/true` 保持在每条出现记录上；两个相同文本可以分别是完整消息与截断预览。

### 5.2 写入与并发

1. 沿用现有解析和预览截断，再对持久化预览计算 hash；P1 不调整解析结果。
2. 请求内先去重候选文本，再批量插入正文、解析 ID、批量插入消息出现记录。
3. 与 request、session、rollup、archive 索引及 ingest receipt 在同一事务提交。
4. 使用 `ON CONFLICT DO NOTHING` 等批量操作；不能每条消息发数次 SQL，或通过无变化 UPDATE 反复制造正文表 WAL。
5. 获取已有正文时核对长度/内容；检测到不一致或注入测试中的 hash 碰撞时明确失败，不覆盖旧正文。
6. 以固定顺序取得引用所需的行锁，防止与字典 GC 竞争。查找后对象被删除的竞态应由小范围重试处理，不能留下悬空引用。

intern 接口借用 `&[ParsedMessage]`，直接从已有 `content.as_bytes()` 计算 hash；仅在 SQLx 绑定确实需要所有权时分配，不 clone 整组消息。返回命名的 `InternedMessages`，明确每个输入位置对应的 `MessageContentId`。批量映射必须检查数量和顺序，不能用 `zip` 静默丢弃多出的输入。SQLx 绑定所借用的容器须保持到 await 完成，返回结果不能借用已经离开作用域的去重容器。

不增加无界内存 hash 缓存。只有基准证明需要时才考虑可丢弃、有容量限制的 ID 缓存，数据库唯一约束始终是正确性依据。

### 5.3 读取与搜索兼容

将 `trace_messages` 改成兼容投影，字段及顺序显式列出：

```sql
SELECT m.id, m.request_id, m.session_id, m.role,
       COALESCE(m.content, c.content) AS content,
       m.created_at, m.content_truncated
FROM session_messages m
LEFT JOIN message_contents c ON c.id = m.content_id;
```

预览导出、完整导出的 `message_previews`、结构化 messages 查询通过该兼容投影获取正文。会话详情先对覆盖索引中的 ID/时间键分页，再按主键读取当前页出现记录并应用相同的 COALESCE 投影，避免深分页时加载已跳过行的 heap 和字典正文。完整 transcript 继续从 raw body 重建，不改成依赖预览。

缺失字典正文是存储完整性错误，不应静默返回空字符串。数据完整性 API 追加悬空引用、双写不一致和回填未完成计数；现有关系检查仍可直接检查出现表的外键列。

若 messages 子串搜索的基准要求索引，可对唯一正文建立 trigram GIN。过渡期旧 `content` 尚未归一化时必须覆盖旧行与新行，不能只搜索字典而漏掉旧数据。实现可显式分两路查询出现记录再合并，也可等待归一化完成后启用优化路径；不能先对两路分别 LIMIT 再认为结果完整。

### 5.4 字典回收

请求过期仍按现有外键删除消息出现记录；正文延迟由现有维护任务批量回收。采用“锁定候选正文，再在当前事务重新确认不存在引用后删除”的方式，与写入端引用锁配合，不单靠可漂移的 refcount。

回收只处理无引用正文；重复文本仍被任何请求引用时必须保留。支持重复执行、并发入库、请求删除与维护中断。第一版不引入跨实例独立 GC 服务。

### 5.5 迁移与回滚

1. 新增表、`content_id`、外键及兼容读取；仍写旧 `content`。
2. 部署支持新模型的所有写入/读取实例，开始事务性双写。
3. 按消息 ID 分批回填，维护高水位、状态、错误和速率限制；每批提交后才能前移进度。
4. 不以 `SKIP LOCKED` 跳过行后直接前移高水位。因删除而不存在的行可跳过；因锁或错误未完成的行必须重试。结束前再次扫描缺失引用和晚提交行。
5. 核对数量、文本、截断状态、分页及导出；确认没有旧 writer 后，才开启仅引用写入。
6. 按批次将已验证的旧 `content` 设为 NULL，实际释放重复 TOAST；保留列供兼容回退。
7. 单独处理 VACUUM、ANALYZE 与必要的物理空间回收。大表重写需要维护窗口和临时空间，不能把逻辑清理等同于操作系统立刻回收空间。

写入模式使用 `MessageWriteMode::{Legacy, DualWrite, ReferencesOnly}`，不使用“是否双写/是否清空/是否去重”等多个布尔开关组合状态。模式切换调用显式校验函数，检查 schema、所有实例版本、回填与一致性结果；模式枚举本身不等于满足这些条件。首版用具体的迁移记录和函数，不开发通用迁移状态机框架。

执行仅引用写入之前，迁移必须显式放宽旧 `content` 的 NOT NULL，并启用“正文或有效引用至少存在一个”的 CHECK；不能靠应用约定绕过数据库约束。大表新增外键/CHECK 可分新增与验证步骤控制锁和扫描，验证通过才允许旧正文清理。迁移命令不得默认连接示例之外的生产数据库。

双写/回填阶段的峰值至少包含旧正文、新唯一正文、引用索引及额外 WAL；物理重写还可能需要一份临时表。上线前计算并设置可用空间停止阈值，低于阈值暂停派生回填而不是继续挤压采集。若收益不达准入要求，应结束双写并撤销派生引用/字典，不能长期停留在增加占用的验证状态。

清空旧正文之前，可停止双写并回到旧读取；清空之后，**回滚旧二进制前必须先从字典重建旧列并校验**。仅关闭 feature flag 不能让旧程序读取已经 NULL 的正文。回滚也需要磁盘余量。

## 6. P2 标签索引与协议处理

当前交付支持从 **Admin → System → Prompt tags** 设置需要索引的名称，初始列表为空。下文以用户启用 `PROJECT` 为例。当前 API 使用 `prompt_tags`、`prompt_tag.name` 和 `prompt_tag.value`。配置变更及历史请求重建见[运维说明](storage-query-operations.md#p2-索引控制与恢复)。

### 6.1 查询语义

核心查询定义为：在当前保留的请求中，存在一个请求侧的可识别对话文本字段，其中包含一个符合标签语法的标记，且其值满足条件。查询匹配结果映射到 session，session 只返回一次。

- 默认覆盖请求输入的 system、developer、user、历史 assistant 和 tool-result 文本；role 可以进一步过滤。
- 不把当前请求的响应、工具定义、函数参数、URL 或二进制 base64 自动视为 prompt 文本。
- 请求里的历史 assistant 内容与当前生成的 assistant 响应必须区分，不能直接复用已混合 request/response 的 `ParsedMessage` 列表推断来源。
- 标签只表示文本中出现了该语法，不证明标记不是引用、代码示例或用户粘贴内容。
- `[PROJECT: atlas]` 的标签名是 `PROJECT`，值是 `atlas`。UI 明确区分“标签值”与“消息预览包含整个标记”。

不修改现有 LLM usage parser 的统计口径。新增纯叶模块 `prompt_tags.rs`，包含 request-only 文本遍历和标签扫描，共用有明确契约的辅助函数；不把 Rust parser、前端 transcript parser 和存储 hash 归一化强行合成一套抽象。它不依赖 storage、AppState、数据库或异步运行时；协议以明确的 enum/match 分发，不为每种协议定义一个只有单一实现的 trait。

worker 将一份拥有所有权的 request body 移入 `spawn_blocking`；闭包内借用字节解析 JSON，再借用已解码字段扫描，只把预算内的标签值和来源位置变成拥有所有权的结果。返回值不能借用闭包内的 `serde_json::Value`；也不为满足 `'static` 把整段 body clone 成多份或包装成共享可变对象。

### 6.2 首版标签语法

| 项目 | 规则 |
| --- | --- |
| 标记 | `[名称: 值]`。名称必须已启用，匹配区分大小写 |
| 名称 | `[A-Za-z][A-Za-z0-9_-]{0,31}`，最多启用 32 个，由管理员配置 |
| 匹配范围 | 单个已解码文本字段/文本 block 内，不跨消息、不跨数组元素拼接 |
| 值 | 去掉首尾 ASCII 空白，保留内部空白、换行、Unicode 和大小写；`[PROJECT:]` 的空值合法 |
| 多标记 | 全部按字段顺序、出现顺序记录，不以同值去掉出现事实 |
| 嵌套/转义 | 第一版不支持嵌套括号或特殊转义；候选内出现 `[`、未闭合或不支持形式时记录诊断，不自行猜测 |
| JSON 转义 | 先解码合法 JSON 字符串，例如 `\u005bPROJECT: atlas\u005d` 可以识别 |
| 上限 | 候选：单值 4 KiB、单请求 256 个标记、提取值总计 256 KiB；最终值需经基准确定 |
| 超限 | 标记 partial 并给出原因；不截短长值后声称建立了完整的精确索引 |
| 定位 | 保存 JSON Pointer、字段/标记序号、解码后字段中的 UTF-8 字节范围及提取版本 |

实际实现采用有界线性扫描，不允许用户提交正则。源字节范围和解码文本范围不是一回事，不把 JSON 转义前后的偏移混用。显示原片段时从归档按相同提取版本解析，返回文本片段与命中片段，前端无需把 Rust 字节偏移直接当 JS UTF-16 下标。

字段遍历顺序在同一提取版本内固定，数组保持原序；不依赖 JSON 对象的序列化顺序分配不稳定的序号。遇到无法判断的标签候选或未知文本块时保守标记 partial，而不是为了得到 ready 状态猜测其值。值归一化规则改变时必须增加版本。

### 6.3 协议与边界矩阵

以下“支持”指 P2 要实现并测试的提取范围；原始存储在所有行中仍保持现状，不因索引器不支持而丢弃 trace。

| 协议或输入 | 标签提取规则 | 完整性与保留要求 |
| --- | --- | --- |
| Chat Completions | `messages[].content` 字符串、明确的 text parts，保留 system/developer/user/assistant/tool 来源 | role、数组顺序、空/null 字段不能改写；未知块不做字符串化搜索 |
| Chat 历史工具调用 | tool-role 及旧式 function-role 的文本结果可查；`tool_calls`、`function_call.arguments` 不属于默认 prompt 文本 | 保留原 role；工具参数、call ID、未知字段仍在原始捕获；不能把未调用工具当调用统计 |
| Responses | `instructions`；`input` 字符串、消息对象/数组、显式 input_text/output_text 文本项 | 按具体形状解析，避免重复收集同一字段 |
| Responses 工具结果 | `function_call_output` 的明确文本 output 可以作为请求工具结果文本 | 结构化对象或未知类型不自动全文序列化；标明未覆盖部分 |
| Provider 管理历史 | 当前请求实际携带的文本可以索引 | `previous_response_id`、conversation/item reference 指向的隐藏内容不可推断，不发起上游补取 |
| Anthropic Messages | 顶层 `system` 字符串/文本块，`messages[].content` 文本块、tool_result 中明确文本块 | thinking、签名、tool_use.input、图片等保留但不作为默认 prompt 文本 |
| 传统 Completions | `/completions` 的 `prompt` 字符串或字符串数组 | token-ID 数组、混合未知形状标记未覆盖，不能把数字转换成文本 |
| Embeddings、图像、音频等 | 不在首版标签协议范围 | 原始代理/归档继续工作，状态为 unsupported，不能显示“没有标签” |
| Generic JSON / HTTP | 不用对整个 JSON 或字节做正则 | 未注册适配器时 unsupported；未来按明确字段白名单扩展 |
| SSE 响应 | 不属于请求 prompt 的标签搜索范围 | 保持原始事件字节，覆盖 BOM、CRLF、多 data 行、UTF-8 分片、未知事件、usage、工具增量及缺失终止事件的回放回归测试 |
| WebSocket | 首版不提供连接内逐消息标签搜索 | 保留当前帧捕获语义、帧数/大小限制和二进制元数据；不宣称恢复了未保存的二进制内容 |
| 非 UTF-8、Content-Encoding | 首版只处理有效 UTF-8、identity 编码的受支持请求 JSON | 不改变代理编码；其他编码标记 unsupported，后续解压须另设解码字节/比率/CPU 预算 |
| 截断或无效 JSON | 不用宽松正则从残缺 JSON 造出结构化字段 | 能可靠提取的正命中可保留为 partial，无法解析则不生成确定性负结果 |
| NUL、Unicode | raw body 原样保留；沿用旧预览的 NUL 处理 | 含 NUL 的标签值不写入 text 精确索引并标记 partial；不能删掉 NUL 后声称值相同 |
| 重复 JSON key | 不改变现有 JSON 解析器的键选择规则 | raw 字节不规范化；索引口径记录为解析后的有效字段，不声称检查了被覆盖的重复键 |

输入请求完整而响应 SSE 中断时，输入标签索引仍可 ready。请求捕获完整性、响应流完整性、provider 外部历史、预览截断是不同维度，不能共用一个布尔值。

协议识别结合保存的 request kind、去掉 query 的请求路径和请求头。当前 `RequestMetadata.content_type` 来自响应，不能用它判定请求是不是 JSON；请求 Content-Type/Content-Encoding 应读取 `request_headers`。历史头缺失、被额外配置脱敏或无法判断时记录诊断，不能从响应 `text/event-stream` 反推请求格式。

### 6.4 索引模型

```text
note_values
  id, index_version, value_sha256, value, value_bytes
  unique(index_version, value_sha256)

request_notes
  id UUID, request_id, index_version, tag_name
  field_ordinal, match_ordinal, source_path, source_kind, role
  note_value_id, decoded_start_byte, decoded_end_byte
  unique(request_id, index_version, field_ordinal, match_ordinal)

request_prompt_index_state
  request_id, index_version, tag_revision, state
  capture_complete, external_history, diagnostics
  indexed_at, attempt_count, next_retry_at, lease_until, lease_token
  primary key(request_id, index_version)
```

标签值只存唯一的小文本，不复制整段 prompt，也不把 request 的用户、API key、model、session 和时间重复存入每个标签。查询通过请求/会话元数据关联，用户归属沿用现有会话口径；这不是“历史身份快照”。

精确值检索使用 `index_version + value_sha256` 定位，并以原值比较复核。值可能长于 B-tree 可接受的单项大小，因此不对任意长 `value` 直接建唯一 B-tree。子串查询使用唯一值上的 trigram GIN，并保留最终 ILIKE 复核。

外键跟随 request 生命周期删除出现记录和索引状态；唯一值延迟按引用回收。若将来标签要比请求元数据保留更久，需要明确的独立事实保留模型，不能一边级联删除来源一边承诺长期可查。首版两者保留期一致。

### 6.5 可恢复索引 worker

为避免可选派生索引失败阻塞原始审计，P2 使用数据库持久 pending 状态和一个受限 worker，不增加外部队列：

1. 标签列表非空时，录制事务用当前配置版本创建 pending 任务。
2. worker 以短事务领取少量任务并写 lease，不在读归档期间长期占用行锁。
3. 通过共享 body reader 的 request-only 选择读取、校验正文；不额外加载 response。旧 segment 内成对压缩时可能仍需解压同一 segment，但不复制无用响应结果。
4. 受限解析，生成标签结果；在一个事务中写值/出现记录及最终状态。
5. 任务在读取期间被 retention 删除时，不重建请求或状态；请求仍存在才允许发布结果。
6. 重启后过期 lease 可重试；唯一键保证重复消费不重复记录。同版本旧结果不能被失败尝试清空。
7. 归档暂时不可读可限速重试；永久缺失/格式不支持明确结束，状态与诊断可见。正文到期后也不伪装成扫描完成。

领取时生成新的 lease token；发布事务检查 token、配置版本和请求是否仍有效。旧 worker 在 lease 过期后即使恢复运行，也不能覆盖新 worker 的结果或删除其标签。唯一值回收复用 P1 的引用安全规则，避免为标签再实现一套有不同竞态的字典 GC；这不要求开发接受任意表名的泛型 GC 引擎。

首版需要再次读取请求归档，这是可量化的额外 I/O/CPU；索引 worker 低于采集优先级，单独限制连接与并发。若基准显示重复解析过重，再设计复用后台已解析字段的优化，不先增加第二条易丢失的内存队列。

状态契约：`pending/processing` 尚未完成；`ready` 表示当前声明范围已扫描；`partial` 表示只覆盖部分；`unsupported` 表示协议/编码不支持；`failed` 表示无法完成。`ready` 不等于拥有 provider 的完整会话历史。

数据库字段先解码为私有的 row 类型，再校验成 `IndexJobState`。`Processing` 必须携带有效的 token/deadline，终态不能附带仍有效的 lease；不能把一个 status 字符串和若干 `Option` 直接流转给业务函数。领取返回 `Result<Option<ClaimedNoteJob>, IndexStoreError>`；None 仅表示暂无任务。发布消费该 claim，返回 `PublishOutcome::{Published, LeaseLost, SourceRemoved}`，后两者是正常并发结果，不是 500 或解析失败。详细生命周期见第 11.5 节。

历史数据通过同一 worker 回填，版本固定、进度可恢复。新提取版本并行回填，通过显式 active version 切换；不把不同版本的行混合后去重。标签配置变更后，查询只读取当前配置版本下的命中。历史回填开启时按批重建，旧游标须重新查询。

## 7. 后端结构化查询设计

### 7.1 查 session 与查命中明细分开

- `sessions` dataset 新增固定命名空间 `prompt_tag.*`，只用于关联过滤。
- 新增 `prompt_tags` dataset，用于查看命中的请求、值、来源和索引状态。
- 现有 `messages.content CONTAINS` 继续表示预览子串；不得静默改成完整 prompt 搜索。
- 新增字段与操作符由 schema discovery 描述，UI 不能自行拼白名单 SQL。

示例请求，保持现有 filters 数组形态：

```json
{
  "dataset": "sessions",
  "fields": ["id", "last_seen", "user_name"],
  "filters": [
    {"field": "prompt_tag.name", "op": "eq", "value": "PROJECT"},
    {"field": "prompt_tag.value", "op": "eq", "value": "atlas"},
    {"field": "prompt_tag.observed_at", "op": "gte", "value": "2026-11-01T00:00:00Z"}
  ],
  "order_by": [{"field": "last_seen", "direction": "desc"}],
  "limit": 100
}
```

等价 Code 模式：

```sql
SELECT id, last_seen, user_name
FROM sessions
WHERE "prompt_tag.name" = 'PROJECT'
  AND "prompt_tag.value" = 'atlas'
  AND "prompt_tag.observed_at" >= '2026-11-01T00:00:00Z'
ORDER BY last_seen DESC
LIMIT 100;
```

`prompt_tag.*` 不是用户 JOIN 或可投影列。所有该前缀条件编译到**同一个标签出现记录**的 EXISTS 中，防止一个标签满足值、另一个标签满足时间或 role 却被合并命中。普通 session 条件仍在外层。

`name` 支持精确匹配，`value` 支持 eq、contains、is_not_null；`role`、`model`、`api_key_hash` 支持 eq；`observed_at` 支持范围比较，时间指来源请求的 `started_at`。model/API key 条件也位于同一个 EXISTS，必须来自携带该标签的请求。`is_not_null` 表示存在符合其他关联条件的标签。暂不开放 `is_null/ne` 等否定关联查询，以免将未索引数据误当作确定不存在。

`prompt_tags` 可返回 `id`、`request_id`、`session_id`、`observed_at`、`role`、`source_path`、`tag_name`、`tag_value`、`index_version`，以及通过固定投影关联的 `model`、`request_kind`、`api_key_hash`、`user_id`、`user_name` 和请求捕获完整性。用户字段沿用当前会话归属口径。后端一次受控查询取得所选字段，不让前端为每一行追加请求补齐元数据。默认列不含整段 prompt。命中上下文通过受保护的小型按需读取动作加载，正文不可用时保留标签命中与来源信息并明确说明。

### 7.2 查询编译和索引

内部流程限定为 `deserialize -> validate -> compile -> execute -> encode`。在边界保留现有 `StructuredQuery`/`serde_json::Value` 请求形状，校验后转成私有的 `ValidatedQuery` 和 `SessionNoteFilter`；后者用有界的 `Vec<NotePredicate>` 保留全部 AND 条件，不按字段塞入 Map 后覆盖重复条件。标签值、时间、UUID 与操作符在编译 SQL 前已经转成对应类型，编译器不再从任意 JSON 临时猜类型。

`SessionNoteFilter` 只有固定关系和具体谓词 enum，不建立通用 SQL AST。构造函数和字段保持模块内可见，调用方不能绕过校验直接构造任意 relation/SQL fragment。该路径可放入 `storage/query/session_note_filter.rs` 叶模块；现有 query.rs 仅做接线，不顺带搬迁所有旧查询代码。

字段与操作符信息使用普通静态表和函数提供给 schema，不用自定义宏生成一套查询语言，也不在 TypeScript 手写第二份操作符规则。协议 JSON、用户 filter 值和动态投影 rows 可以使用 `Value`；分页、coverage、诊断和错误 envelope 使用命名的 serde 类型，避免把所有内部结果都变成字符串键 JSON。

示意 SQL：

```sql
SELECT s.id, s.last_seen, s.user_name
FROM trace_sessions s
WHERE EXISTS (
    SELECT 1
    FROM trace_prompt_tags pt
    WHERE pt.session_id = s.id
      AND pt.index_version = $1 AND pt.tag_revision = $2
      AND pt.tag_name = $3 AND pt.note_value_id = $4
      AND pt.tag_value = $5 AND pt.observed_at >= $6
)
ORDER BY s.last_seen DESC, s.id DESC
LIMIT $7;
```

这只是语义示意，不预设 EXISTS 一定是最快计划。对低选择性和高选择性条件，实测比较从标签值出发的 DISTINCT session 子查询及半连接；不在候选命中记录上先 LIMIT 再做 session 去重。

最小索引集合：

- `note_values(index_version, value_sha256)`，唯一；可选 `value gin_trgm_ops`。
- `request_notes(tag_name, note_value_id, request_id)` 用于按标签名和值查询，保留按 request/version 的唯一定位索引。
- 复用现有 request/session 时间索引；仅经计划证明需要才增加 session `(last_seen, id)` 等复合索引。
- `session_messages(content_id)` 支持字典命中到出现记录和无引用检查。

trigram 只能减少候选，仍须复核括号、冒号、大小写及整个子串条件；不使用相似度 `%` 代替包含。短值、仅符号或高频值可能退化，不能承诺任何 contains 都快。`pg_trgm` 和有效索引未就绪时，schema 不广告高效子串能力；精确查询独立可用。

`FieldSpec`/schema 增加“仅过滤字段”的独立描述，例如 `related_filter_fields`。现有 `fields` 继续仅列可投影字段，`SELECT *` 不包含关联虚拟字段；Code 解析和 Builder 分别按投影、过滤、排序用途校验字段。

### 7.3 分页、一致性和导出

- 新增 `page.has_more`，通过多取一条判断；新增 `next_cursor`，不计算昂贵的精确总数。
- sessions 以 `(last_seen, id)`、命中明细以 `(observed_at, request_id, id)` 作为明确支持的游标排序；内部自动补唯一键，不要求该键出现在投影中。
- 任意排序不自动宣称支持 cursor；schema 暴露支持的排序，拒绝不匹配/被篡改的游标。游标携带 dataset、条件指纹、排序和提取版本，并受服务端校验。
- 现有 OFFSET 接口保持兼容；新增搜索 UI 优先使用 cursor。
- **cursor 不等于跨页数据库快照。** session.last_seen 会更新，journal 会晚到，回填会增加历史命中，retention 会删除数据。UI 显示实时结果、同 ID 去重并允许刷新；不能把扫到空页宣称为整个历史已完整对账。
- `as_of`/时间上限不能解决晚提交可见性，不能用它冒充 repeatable-read。
- 当前 JSONL query export 仍是有限行结果，需显示导出范围。需要严格一致的全量搜索导出时，应另做有配额的快照流式任务；不是首版隐含能力。现有完整 session export 的快照与 end marker 继续保留。

标签查询的有限行 JSONL 导出不添加会破坏旧消费方的元数据行。通过新增导出响应头传递 `has_more`、coverage 与 index version，前端明确提示“导出的是本次有限结果，覆盖不足/仍有更多”；不能仅提示导出 N 行成功而丢失这些限制。复制数据、切换模式和导出均使用同一份已验证查询。

### 7.4 覆盖状态和资源预算

响应在原有 dataset/fields/rows/limit 之外增加可选元数据：

```json
{
  "page": {"has_more": true, "next_cursor": "opaque-cursor"},
  "search": {
    "scope": "captured_request_prompt_tags",
    "index_version": 1,
    "coverage": "partial",
    "coverage_checked_at": "2026-11-16T00:00:00Z",
    "coverage_counts_exact": false,
    "warnings": ["部分请求尚未索引；零命中不代表不存在"]
  }
}
```

覆盖按用户可访问的请求时间范围统计，区分 pending、partial、unsupported、failed、missing source。全量精确计数可能昂贵：允许返回有时效/范围声明的维护摘要或 `unknown`，不为每个搜索同步扫描全部状态。只能在确有证据时返回 complete；正命中不因覆盖不足被隐藏。

查询设置并发槽、连接获取时限、总 deadline、SQL statement timeout、返回字节预算。总 deadline 覆盖连接等待、事务和编码；输出超预算明确失败，不静默截断某一行。输入 filter 值大小限制继续保留。

索引 worker、回填与交互搜索分别限流；采集写入优先。若引入独立 read pool，必须实际创建独立连接池，`PgPool.clone()` 不构成隔离；同时控制数据库总连接数。容量不足返回 429，临时依赖不可用返回 503，查询预算耗尽返回稳定的超时错误码，校验失败返回 400；错误中不泄露 SQL 或 prompt。

浏览器 Abort 不等于数据库已经取消执行。服务端须验证取消/deadline 后事务、连接和并发槽确实释放，并保留 SQL 端超时作为上界；不能只通过按钮恢复可点击来证明取消成功。

## 8. 前端 UI 修改与可靠性

### 8.1 用户操作闭环

1. 在 **Admin → System → Prompt tags** 保存要启用的名称。
2. 在 **Sessions → Filter by prompt tag** 选择标签名，再按精确值、包含或存在筛选，可选请求时间和 role。
3. UI 发送与 Query Builder 相同的结构化查询，不在浏览器下载所有消息或归档做过滤。
4. 后端返回去重 session 行、has_more、cursor 和覆盖状态；session 不按命中条数重复展示。
5. 点击“命中明细”打开 `prompt_tags` 查询，展示相关 request、值、时间、role 和来源字段。
6. 点击来源可到 Request/Transcript；只有展开上下文才读取原始 body。正文已删除/缺失时有明确提示，不能显示空白冒充空 prompt。

Sessions 页面和 Query 页面复用同一个 typed request 构建 helper；Builder 与 Code 必须可以往返转换标签过滤。关联字段不可作为普通列勾选或排序。新版字段只有在 schema 宣告支持时才展示，连接旧服务器时不发送新过滤。

### 8.2 三类搜索明确区分

| UI 模式 | 范围 | 用户能得出的结论 |
| --- | --- | --- |
| 消息预览包含 | 现有、有限长度的持久化预览 | 命中有效；未命中不证明原始 prompt 无此内容 |
| 输入 prompt 标签 | 支持协议中已捕获、已索引的请求文本 | 符合声明的标签语法；同时看索引覆盖和捕获限制 |
| 原始正文查看 | 单个请求当前可用归档 | 展示当前捕获，不自动扩大为全库全文搜索 |

不在 UI 放置尚未实现的“全量完整 prompt 搜索”开关。未来需要任意全文时再评估唯一文档索引，不能悄悄给每次请求增加另一份全文。

### 8.3 输入、结果和异步状态

- Builder、Code、JSON literal 共用安全整数和大小校验；拒绝超 JS 安全范围的整数，不静默取整。已有 API 的 i64 数值类型不在本方案中偷偷改成字符串；超安全范围结果提示无法精确表示，游标不依赖浏览器解析 ID/数值。
- RFC3339 时间必须带时区；保留精度及 offset，查询往返不自动丢失微秒。统一显式与省略 limit 的规则。
- LIKE 模式中的 `%`、`_`、反斜杠按字面转义；值里的引号、点、方括号、emoji 和换行均有回归测试。
- 空标签值与未填写条件不同；输入框不能把合法空值直接丢弃。
- run、export、分页统一 busy 状态，使用 AbortController；离开页面、切换条件取消旧请求，并使用请求序号防止迟到结果覆盖新结果。
- 请求失败保留上次结果时，明确显示“上次查询结果”和条件，不能把旧结果标成当前过滤的结果。
- 分页 cursor 与原查询指纹绑定；更改 dataset、过滤、排序或提取版本后重置分页。
- 高亮使用文本节点/受控片段，不将 prompt 注入 `innerHTML`。导出、复制与屏幕文本一致；片段不等于完整标签源字段。
- pending/unsupported/partial 与“0 个匹配 session”同时显示；只有覆盖充分时才显示明确的否定提示。

## 9. P3 无损分块归档的条件方案

### 9.1 实施前必须证明的收益

对完整连续会话比较：现有逐请求压缩、较高 zstd 等级、内容分块+独立压缩。计入 chunk/manifest 表、索引、pack 空洞、GC 临时空间及恢复开销。若净收益不足，P3 不进入实施；P1、P2 仍独立交付。

分块必须基于未压缩的原始 body，不能对包含 trace_id 的 frame 或压缩结果计算“语义正文 hash”来期待跨请求命中。未知协议、无效 JSON、SSE 和二进制捕获也按字节处理，不需要增加协议依赖。

### 9.2 格式与读取

```text
body_manifest(trace_id, direction, format_version,
              captured_len, body_sha256, complete, ordered_chunk_refs)
chunk(hash, uncompressed_len, codec, pack_id, offset, compressed_len)
pack(id, storage_backend, storage_key, checksum, state)
```

每个 manifest 独立描述完整捕获；不引用“前一个请求+差量”。块之间按序拼接后必须与原 body 的长度/hash 相同，零长正文单独表示。预览截断、HTTP 捕获截断、归档缺失三个状态不合并。

块分别压缩，pack 不可变；候选平均块大小与 pack 大小从样本确定，不把 32 MiB 作为默认最优值。小正文可直接存储。解码限制在 body、单块、manifest 引用数等层面校验，防止损坏引用导致越界、超大分配或循环结构。

所有详情与导出通过 `bodies.rs` 分发 v1/v2；客户端和 JSONL schema 不需要知道存储版本。API 对 raw 字节的 utf8/base64 编码保持不变。

### 9.3 持久化、幂等与垃圾回收

- journal 仍先持久化原始捕获；pack 完整写入、校验并同步后，数据库才提交引用与 receipt，之后才能 ack journal。
- pack 拼装只在后台进行，批量同时设字节和时间边界；不能等待大 pack 填满才无限期入库。
- 数据库事务失败允许留下可识别孤儿 pack，重试保持幂等；不会因重复处理增加统计或重复引用。
- manifest 是引用真相，refcount 如存在仅是加速信息；GC 必须防止与新引用、旧快照和导出竞争。
- 读 lease/pin 在跨实例下可识别；不能只用进程内计数保护共享文件。过期 lease、长导出、实例死亡都有测试。
- pack 整理先写新文件、事务切换位置、等待旧读者退出后删旧文件；共享块在最后一个保留引用消失前不能回收。
- 指标同时统计 live chunk、pack 物理量、死空间和待回收量；不能把逻辑引用字节当实际容量。

### 9.4 迁移和回退

新格式先 opt-in，读端同时支持旧 inline、旧 segment 和 v2。历史转换按 body 写新 manifest，完整恢复并核验后，才切换索引；旧副本延迟回收。正文 hash、截断标记和导出数量任何一项不一致即停止该批转换。

停止 v2 新写不等于旧二进制可读取已有 v2。回退旧二进制需要先物化为兼容归档并验证，或者继续使用兼容 v2 的读取版本。转换、回退和 pack 整理均须预留峰值空间。

## 10. P4 冷热分层与生命周期

冷热首先改变位置，不改变业务保留期。例如完整正文要求 30 天时，可以本地只缓存近期数据，但 30 天内的持久归档仍可按需读取。7 天后彻底删除正文则无法保持 30 天完整回放，必须作为独立保留策略审阅。

新增本地缓存容量不能复用现有 `rotate_size_bytes` 的删除含义。拟议区分本地缓存上限、完整正文保留期、metadata/索引保留期；旧配置继续保持原语义，显式迁移后才启用新模式，避免升级后意外删除或无限保留。

远端发布至少经过 upload、checksum 验证、位置提交、本地可驱逐四步。上传失败、断网、远端丢失、磁盘满时不得删除唯一有效副本；缓存 miss、真正 missing、暂时 unavailable 分开呈现。NAS 挂载失败时不能把文件写进挂载点下面的本地目录并误认为已持久化到 NAS。

远端读取设置时限、并发和范围读取预算；大导出继续流式，取消应释放连接/lease。远端不可用时 metadata 与标签查询仍应可用，原始正文查看明确返回依赖不可用，不能伪装为空内容。

正文已到期而 metadata 继续保留时，UI 必须显示正文已过期；会话统计和标签的保留口径另行定义。不得把新增留存周期当成本方案默认已获得批准的生产配置。

## 11. 修改位置和交付边界

| 位置 | 拟议修改 |
| --- | --- |
| `migrations/` | P1 字典、引用列/约束、兼容 view；P2 标签值/出现/状态；P3 另行版本化 schema |
| `storage/message_contents.rs`（新增） | 批量 intern、引用校验、有限 GC；不增加通用 repository trait |
| `storage/ingest.rs` | 同事务写字典引用；P2 创建索引 pending，不同步读取归档 |
| `storage/models.rs` | 保持现有 read model JSON；新增 typed 标签/coverage/page 响应 |
| `storage/sessions.rs`、`session_export.rs`、`query.rs` | 统一预览读取；标签白名单关联过滤和结果编码 |
| `storage/analytics.rs`、`retention.rs` | 完整性、字典/索引容量、维护和生命周期；不重复计算请求统计 |
| `prompt_tags.rs`（新增叶模块） | `NoteExtraction`、协议分发和受限扫描；纯同步函数，不依赖存储 |
| `storage/note_index.rs`（新增 facade） | 仅声明子模块、显式重导出必要接口；不再堆积 worker 与 SQL 实现 |
| `storage/note_index/note_index_store.rs`（新增叶模块） | SQLx row 解码、claim、幂等发布、状态和引用查询 |
| `storage/note_index/note_index_worker.rs`（新增叶模块） | `NoteIndexWorker`、并发预算、重试和显式 shutdown；不实现 SQL 细节 |
| `storage/query/session_note_filter.rs`（新增叶模块） | 标签关联过滤的类型化校验和 SQL 追加；不提供任意 JOIN |
| `storage/bodies.rs` | P2 request-only 读取选项，P3 版本分发；保持一套校验/解码入口 |
| `main.rs`、`config.rs`、`metrics.rs` | 有界 worker 生命周期、受验证参数、停止/回填/查询指标 |
| `api.rs` | typed 查询错误、能力和覆盖信息、按需上下文；复用当前认证和 body 限制 |
| `ui/src/lib/api/` | types、endpoint wrappers、共享 fixtures 同步 |
| `ui/src/lib/utils/query*.ts` | 分用途字段校验、模式往返、安全整数、标签字段补全 |
| Query / Sessions 页面 | 同源查询 helper、标签控件、分页、取消、命中明细与覆盖状态 |
| `docs/` | query-editor、session-export、transcript、运维迁移与性能报告 |

本次使用 008–011，不覆盖 001–007。大表索引如需 CONCURRENTLY，应走明确的非事务维护步骤，不能直接塞进默认事务迁移；新空表上的索引可随事务迁移创建。

### 11.1 Google Rust Guide 的适用范围

[Google Rust Guide](https://google.github.io/styleguide/rustguide.html) 的应用/库区分用于控制首轮抽象成本。其借用、命名字段、类型、错误和文档原则应用到下面的接口约束。Chromium/Fuchsia 的专用线程、async、构建布局和测试设施不迁入本项目；继续使用 Tokio、Cargo 和现有测试框架。应用内类型也不因拼写为 `pub` 就一律加 `non_exhaustive`。

这里的类型与流程是针对本项目的实现决策，不能将数据库 fencing、GC 锁协议或性能门槛声称为 guide 直接规定的算法。

### 11.2 模块、可见性和依赖

- 新增非叶模块只组织子模块和显式重导出；已有 storage.rs、trace.rs 的历史组织问题不作为本次大范围重构前提。
- 纯提取器暴露给本 crate 的必要接口用 `pub(crate)`；存储 helper 默认私有或 `pub(super)`。确需经 facade 向外提供的条目单独设置足够且最小的可见性，不用重导出绕过原权限，也不将 SQLx row、原始 lease 字段和事务 helper 全部导出。
- 新生产模块显式导入依赖，不延续 `use super::*` 或新建 prelude。一个 use 导入一个模块的条目；不为稳定构建引入 nightly-only rustfmt 配置。
- 调用其他模块的自由函数带模块名，例如 `message_contents::intern_batch`；类型就近导入。少量方法使用的 trait 在函数内导入。
- 表名允许复数，主 Rust 类型/文件使用清楚的领域名；避免到处定义无含义的 `Context`、`Manager`、`ResultTuple`。
- 继续复用 serde/SQLx/thiserror derive；不自建字段注册宏、协议宏或泛型 repository。只有真实的多实现/泛型调用需求才引入 trait，不能仅为 mock 或整理文件创建接口层。

### 11.3 类型和函数契约

新增代码优先约束容易混淆、可能出现非法组合的边界，而不是给所有标量包一层类型：

| 边界 | 实现约束 |
| --- | --- |
| 正文 ID 与消息 ID | `MessageContentId(i64)` 区分字典主键与原有消息序号；SQLx 转换在存储边界完成 |
| 请求与 lease | 新 worker 使用不同的 `RequestId`、`LeaseToken` newtype，不能都以裸 Uuid 传给相邻参数；不要求改写所有旧接口 |
| 指纹 | `ContentHash([u8; 32])` 表达固定字节数；不要在 Rust 层流转可任意长度的十六进制 String |
| 版本 | `NoteIndexVersion` 仅由已支持版本解析得到；未知数据库版本返回明确错误，不能默认为最新 |
| 文本坐标 | `DecodedTextSpan` 私有构造，校验范围及 UTF-8 字符边界；命名必须区分解码文本字节与 raw JSON 坐标 |
| 历史预览状态 | 内部使用 Complete/Truncated/UnknownLegacy，边界显式映射到旧 `Option<bool>`，不让业务靠 `unwrap_or(false)` 抹掉 unknown |
| 写入/读取选择 | `MessageWriteMode` 与 `BodySelection::{RequestOnly, Both}` 代替含义不清的布尔参数；HTTP 现有布尔字段保持 wire 兼容 |
| 批次结果 | `InternedMessages`、`NoteExtraction`、`QueryPage` 等命名结构体；不返回 `(Vec<_>, bool, Option<_>, Option<_>)` |
| 校验后查询 | `ValidatedQuery`/`SessionNoteFilter` 字段私有，只能从校验入口生成；SQL 编译不接受用户 SQL 字符串 |

用于启动配置的 `NoteIndexOptions` 作为参数结构体提供可用的 Default。再经 `try_new`/`TryFrom` 生成字段私有的有效 limits；并发数、批次大小不能为零，字节/超时有上界，单值预算不能超过总预算。配置错误明确失败，不通过 clamp 静默改变用户意图。含资源或不存在合理默认值的 worker/claim 不实现 Default。

普通接口使用 `&str`、`&[T]`、`Option<&T>`；不新增 `&String`、`&Vec<T>` 或应用层不必要的 `impl Into<String>`。有界批次天然要存入数据库时返回 Vec 即可，不为风格强迫改成复杂借用迭代器。

按使用需求 derive Eq/Hash/Copy 等；资源句柄和 claim 不自动 derive Clone，含 prompt/标签正文的 DTO 不自动 derive Debug。纯转换或必须使用的结果适当加 must_use；返回 Result/Option 的函数不重复堆叠无收益的属性。新增 fallible 构造器用 `try_new`，不趁机全库重命名既有 `new`。

### 11.4 错误、普通结果和诊断

| 情况 | 表达与处理 |
| --- | --- |
| 暂无任务、请求已不存在 | `Result<Option<T>, E>` 的 None；数据库失败不能伪装成 None |
| 不支持协议、捕获不全、扫描超预算 | `ExtractionOutcome`/coverage/reason enum 的普通结果，继续保存原 trace；不通过 panic 或空 Vec 丢掉原因 |
| lease 失效、来源被删 | `PublishOutcome`，不当成会阻塞队列的永久故障 |
| 字典正文不匹配、数据库行状态非法 | `MessageStorageError`/`IndexStoreError` 的明确变体，停止错误写入并可定位到 ID |
| 数据库/归档 I/O | 保留 source 的 Result；由 worker 明确判定有限重试，由应用边界添加 anyhow 上下文 |
| 查询参数、超时、容量、依赖错误 | 校验/执行错误类型映射为稳定 API code 和 HTTP 状态，不靠匹配错误文案分类 |

错误类型只在调用方确实要分类的模块边界定义，沿用 thiserror/anyhow，不为每个私有函数创建一套 error enum。reason 和 error code 稳定序列化，显示文案独立；TypeScript 用可辨识联合处理状态，并为未知新版本显示明确的兼容提示。

外部 JSON、损坏归档和错误数据库行均用 Result/match 处理。新路径不使用 `Row::get` 或 unwrap 将数据问题变成 panic；优先 `try_get`、`try_from` 和模式匹配。仅不可避免的程序不变量才允许带原因的 expect。诊断只记录 request ID、阶段、版本、reason 和必要计数，不拼接原 prompt/标签、secret header 或完整 SQL 参数。

### 11.5 所有权、租约和任务关闭

`NoteIndexWorker` 自己拥有调度循环、已启动任务和关闭信号；task 边界只转移任务需要的 body、元数据和少量 limits。PgPool 的轻量 clone 和确有共享需求的 Arc 保留，但不增加 `Arc<Mutex<AppState>>`、共享 JSON body 或跨 await 持有的同步锁。

`ClaimedNoteJob` 是字段私有、must_use、非 Clone/Copy 的拥有所有权对象，只能由 store 的领取函数生成。发布函数消费它，并以数据库 token/version 条件作最终 fencing。Rust 的所有权防止同一条进程内路径误复用 claim，不能替代数据库在 lease 过期后的并发校验。

```text
短事务领取 -> 释放数据库事务
  -> 读取一份请求正文
  -> spawn_blocking 中受限解析
  -> 短事务校验 fencing 并发布
  -> 释放本地 permit 和任务资源
```

claim 的 Drop 不执行异步 SQL、不宣告完成、不创建 detached 清理任务；放弃的持久任务通过 lease 到期恢复。Semaphore permit、buffer 和数据库事务的本地资源释放使用 RAII，数据库发布/提交仍是显式步骤，不能混为一种保证。

提供显式 `shutdown`，停止领取、通知现有任务、在预算内 join 并返回命名的关闭报告。main 在关闭数据库前等待它；超时/中断时保留未完成 lease 供下次恢复。新增任务不得只 spawn 后丢弃 JoinHandle，也不能通过 std 线程另建一套调度。

`spawn_blocking` 启动后通常不能靠 abort 立即终止，输入/扫描预算必须有硬上界；长遍历在边界检查关闭标记。不得把数据库事务带进 blocking 闭包。JoinError、关闭超时和意外 worker 退出要记录并更新索引健康状态，不能以“已取消 await”声称磁盘/解析工作已经停止。

新路径不需要 unsafe、catch_unwind 或共享可变缓存。若以后确实需要锁，必须说明不变量和 poison 后的明确失败/重建策略，不能用 `PoisonError::into_inner()` 继续信任被污染状态；不修改无关旧模块来凑全库风格一致。

### 11.6 Rust 文档和实现审阅清单

- 叶模块使用 `//!` 说明职责；可见接口用 `///` 写清 body/文本身份、借用有效期、输入上限、失败、取消和 lease 行为，而非重复函数名。
- `try_new`、行解码、状态迁移、模式切换的非法输入必须有单测；未知枚举值不走 `unwrap_or_default`。
- 协议遍历优先清楚的循环和 match，保留来源/诊断；不使用 `filter_map(Result::ok)`、长副作用链或模糊类型推断静默丢字段。
- 测试断言标明协议、字段位置、版本或批次 ID，但不输出真实敏感正文。只测试公共行为与重要不变量，不为了覆盖每个 getter 增加镜像测试。
- 审阅检查新增 pub、Arc、Clone、bool 参数、Value、trait、宏、unsafe 的必要性；没有业务理由的抽象不进入主分支。
- Rustfmt/Clippy 使用仓库固定工具链，不新增全局 allow 来压制问题，也不因风格建议启用 nightly。已有 Rust/SQLx/前端契约测试继续使用原框架。

## 12. 需要重点验证的失败场景

| 场景 | 预期结果 |
| --- | --- |
| 同文本跨 role、session、重试 | 共享正文，出现记录、分支和统计均不合并 |
| NULL/空文本/历史 NULL 截断状态 | JSON 和旧接口语义不变 |
| 并发 intern 与正文 GC | 无悬空引用、误删、死锁；重试有界 |
| journal 重放、事务中断 | trace、rollup、消息出现数及标签发布不重复 |
| worker lease 超时后旧任务恢复 | token 失效的任务不能覆盖新结果；任务可恢复且不重建已删除请求 |
| 回填遇到 retention 或晚提交 | 不复活删除数据，不跳过遗漏行，最终可对账 |
| 一条消息中标签跨 block/嵌套/超限 | 不拼造命中，有版本化诊断与覆盖提示 |
| 多个标签分别满足条件 | 所有相关过滤必须在同一出现记录上成立 |
| 完整请求但响应截断 | 请求标签可 ready；响应保留原来的不完整提示 |
| 捕获截断/不支持编码/未知协议 | 不阻断代理和原始持久化，不把零命中当确定不存在 |
| `%`、`_`、反斜杠、SQL 引号 | 字面匹配和 bind 参数，无用户 SQL 拼接 |
| 汉字、emoji、组合字符、CRLF | 原始字节可恢复；UI 高亮、复制不切坏字符 |
| 超长插件路径和同前缀字段 | 返回键完整且不碰撞 |
| Code/Builder 切换、非法草稿 | 合法请求语义一致；保留当前草稿恢复行为 |
| 用户改条件后旧请求返回 | 不覆盖新结果，不沿用旧 cursor |
| 导出时轮转/GC/远端迁移 | 读保护生效或明确失败；不以 HTTP 200 冒充完整导出 |
| pack/字典损坏、远端丢失 | 明确完整性错误；其他请求/另一方向 body 不被吞掉 |
| 本地容量满且远端不可用 | 不删除唯一副本；限流/积压/丢弃指标准确可见 |
| 非法版本、非法 Processing lease、空批次上限 | 构造/解码拒绝非法状态，不默认成 ready 或无限重试 |
| intern 返回数量/顺序错误、同 hash 异文 | 显式完整性失败，不静默缩短消息列表或覆盖正文 |
| shutdown 时解析仍在 blocking worker 中 | 本地任务可追踪、工作有界；不 ack 未发布任务，lease 可恢复 |

## 13. 测试、性能与发布验收

### 13.1 正确性测试

- Rust 单测覆盖正文身份、碰撞注入、标签语法、所有声明支持的请求形状、覆盖状态和查询校验。
- PostgreSQL 测试覆盖约束、双写/归一化混合读取、并发 GC、回填恢复、关联过滤同一行、session 去重、分页和 retention。
- 保留全部旧消息/详情/导出 fixtures；同一数据集迁移前后比较 JSON，允许差异仅限明确新增的可选元数据。
- 完整导出对每个 body 解码后比较字节、长度和 SHA-256；计数、顺序、truncated、utf8/base64 和最终 end marker 均一致。
- 前端 contract、Builder/Code round-trip、自动补全、条件联动、分页、取消、错误与覆盖状态测试。
- 浏览器端端到端使用真实后端和 PostgreSQL，验证“写入带标签的合成请求 -> 索引 -> UI 搜索 -> session -> request/transcript -> 导出”，不能只用 mock 响应证明查询可用。
- 协议 fixtures 同时包含合法、未知、截断、分支和工具形状；已支持回放能力不得因新索引适配器退化。
- Rust 类型边界覆盖三种写入模式、非法状态行、未知版本、坐标越界、unknown 预览状态和一一批次映射；验证这些错误不会退化成空内容或完整覆盖。
- 用显式屏障/可控时钟测试两个 worker 的 lease 竞争与关闭，不依赖随机 sleep；取消测试同时观察任务、连接和 permit 释放，而不仅检查返回 Future。

### 13.2 性能验收口径

下表列出需要在部署环境用代表性流量验证的验收门槛。

| 指标 | 建议验收方式 |
| --- | --- |
| P1 空间 | 同数据集比较字典+出现表+全部相关索引与旧消息存储；建议净节省至少 30%，否则暂不清空旧正文 |
| P1 写入 | 相同 CPU/内存/数据库限制下，后台持续吞吐不低于基线 90%；不能靠延长积压隐藏退化 |
| 代理与可靠性 | 同负载比较 HTTP/SSE 首字节、p95、RSS；可接受负载下所有 drop/failure 计数仍为零 |
| 标签精确查询 | 代表性约百万请求量级，p95 目标 1 秒以内；同时测罕见、常见和零命中 |
| 标签子串 | 代表性窗口内 p95 目标 2 秒以内；短串/符号/高命中单列结果，不以最佳例子代替总体 |
| 查询隔离 | 至少覆盖 8 个并发交互查询和持续采集；观察连接等待、journal 积压与 readyz |
| 索引新鲜度 | 建议正常负载下 p95 10 秒以内；积压时 UI 显示 pending，不降低正确性口径 |
| 读放大 | 迁移前后测消息详情、深分页、完整会话导出；建议热读取 p95 不退化超过 20% |
| P3 准入 | 计入 manifest/索引/pack 空洞后，相比已调整压缩参数的基线仍有显著净收益，且满足恢复/故障测试 |

使用 `EXPLAIN (ANALYZE, BUFFERS)`，记录 custom/generic prepared plan、冷热缓存、选择性、数据规模与 PostgreSQL 版本。存在索引不代表实际使用，更不代表目标负载一定更快。百万请求不等于百万消息，测试必须记录展开后的出现记录数。

### 13.3 实施后的检查命令

本次已执行以下检查。数据库、插件和浏览器相关的前提及实际测试数见性能报告：

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

# DATABASE_URL 必须是能创建隔离测试库的专用测试实例
cargo test --workspace --locked -- --include-ignored \
  --skip performance_ --skip journal_process_helper --skip openrouter_live

pnpm --dir crates/llmtrace/ui check
pnpm --dir crates/llmtrace/ui test
pnpm --dir crates/llmtrace/ui build
```

数据库/插件集成测试先按现有 CI 构建所需 WASM fixture。使用与 CI/Docker 对齐的 Rust 工具链，不将本地 nightly 的警告差异误判为固定工具链的构建结果。

发布每阶段分别验证 embedded migrations 和旧库升级；新功能未就绪时 schema/UI 不应广告可用能力。回填和物理回收均有暂停、继续、速率控制、剩余容量告警和明确的回退检查点。

## 14. 实施结果与阶段决定

本节记录 11 月 16–17 日首版实测。当时使用固定示例标签；当前可配置标签的查询契约见第 6–8 节。

| 范围 | 实现和验证 | 结论 |
| --- | --- | --- |
| P0 测量 | 显式 `message-maintenance measure`，高/无重复物理空间对比，真实入库三模式、详情/深分页/完整导出、zstd/离线 CDC，百万请求和 compiled EXPLAIN，8 查询客户端并行采集 | 已交付可复跑工具及合成报告；没有采集生产正文或据生产汇总外推压缩比 |
| P1 模型与读写 | 008、`message_contents.rs`、兼容视图与分页投影，精确 UTF-8 身份、同文跨角色/session、三态截断、碰撞/GC 竞争测试 | 共享正文，不合并请求/出现事实；原始字节不重写 |
| P1 运维 | `message_migration.rs` 的 Prepare/Measure/Status/Backfill/三种写入模式/清理/恢复/GC，旧库 additive 升级、并发晚提交回填测试 | 一次一批、容量停止参数、显式门槛确认；从未自动清空生产旧正文 |
| P1 兼容 | 同数据完整 JSONL 迁移前后比较，UTF-8/base64 原字节及 SHA、顺序/ID/NULL/计数/end marker；真实 UI transcript | 旧数据和导出格式保留；旧发布二进制还受 SQLx migration catalog 检查，回退细节见运维文档 |
| P2 提取 | `prompt_tags.rs` 及纯协议 fixtures | 请求侧、单字段扫描，Unicode/空值/重复/嵌套/超预算/取消均有明确语义 |
| P2 索引 | 009/011、独立 pool worker、版本/lease fencing、有限重试、低优先级可暂停历史回填、来源只读/解压预算、字典 GC | 可恢复；失效任务不能覆盖新结果或复活删除的请求；关闭超时返回仍运行任务的所有权，main 显式 join |
| P2 查询 | `prompt_notes`、sessions 单个 EXISTS、短内部输出构造、数组 GIN 操作符、独立查询 pool、预算和稳定错误、010 HMAC cursor | 继续 allowlist + bind，无 raw SQL；精确值先解析字典 ID，使计划器识别常见/罕见值选择性 |
| P2 UI | 共用 Builder/Code/Sessions 校验与 schema，WHERE-only 关联字段、游标去重、取消/旧响应隔离、覆盖提示、按需上下文和请求状态、有限导出 | 真实 PostgreSQL + 代理 + 浏览器验收通过，含规范化后的 transcript 与原始导出；合成截图和报告可审阅 |
| P3 原始归档 | 仅离线 CDC 参数测量；metadata 是估计成本，不是生产 manifest | 不满足完整可靠性与维护成本准入证据，生产归档仍保持现有格式 |
| P4 冷热迁移 | 本次未引入远端依赖或缓存生命周期 | 保留条件设计，未宣称已交付远端读取/故障恢复能力 |

已采用的决策：首轮范围为 P0–P2；原始保留语义不变；NOTE 排除当前响应、工具 schema/参数和未捕获历史；未知覆盖明确显示；先双写验证再显式清理；Rust 按具体叶模块、受限类型和任务所有权实现，不新增通用框架。

上线仍需操作者按代表性流量确认第 13 节的空间/吞吐/延迟、空闲空间、WAL 和全部 writer 版本。低重复率或双写开销不能通过本地最佳结果掩盖；不达门槛应停止派生回填、保留 Legacy/恢复 inline，再结束验证。生产清理的显式确认是部署条件，不是本次已执行的动作。

### 14.1 2026-11-17 最终复核

复核补齐了 NOTE helper 的数据库超时/依赖错误分类、只能经校验构造的提取预算，以及重试耗尽任务不阻塞后续领取的行为。查询逐行读取并检查返回字节预算，避免先在内存中堆积整页大 metadata。UI 按记录身份复用结果行，换行会取消旧 NOTE 上下文，迟到响应不能污染新行。新增回归验证这些路径；全套数据库/进程测试共通过 405 项（389 应用 + 16 插件），前端 207 项、Clippy、rustfmt 和 release 构建均通过。

执行计划使用实际编译的 SELECT 做 `PREPARE / EXPLAIN EXECUTE`，逐次检查 `pg_prepared_statements` 的 custom/generic 计数增量，防止把普通 EXPLAIN 包装误当成 generic plan。新增独立 2 GiB / 4 CPU PostgreSQL 的 shared-buffer 冷缓存样本，以及 8 个 HTTP 查询客户端与真实代理/journal/NOTE worker 同时运行的组合负载。冷缓存测试只重启自己创建的容器；未清空宿主机文件缓存。

深分页的覆盖索引优化通过实际 SQL 的 `EXPLAIN (ANALYZE, BUFFERS)` 验证；维护前后的读延迟分列保留，最后一轮详情、深分页、完整导出的热读均通过本地建议门槛。最终 release 还通过真实浏览器的 50→53 唯一 session、Code/Builder、按需上下文、规范化预览、原始 transcript、有限/完整导出和移动布局复核。生产门槛及早期未通过样本仍保留，不用本地合成结果替代部署确认。

## 15. 参考资料

- [Google Rust Guide](https://google.github.io/styleguide/rustguide.html)：第 11 节的适用范围和 Rust 接口审阅依据；数据库和业务算法仍按本项目不变量验证。
- [现有 durable capture 契约](durable-capture.md)、[完整导出契约](session-export.md)、[Transcript 行为](transcript.md)、[查询编辑器](query-editor.md)。
- [PostgreSQL TOAST](https://www.postgresql.org/docs/16/storage-toast.html)：解释大字段压缩与物理占用，不能据逻辑长度直接承诺磁盘收益。
- [PostgreSQL pg_trgm](https://www.postgresql.org/docs/16/pgtrgm.html)：用于子串候选加速；仍需匹配复核，低选择性和无有效 trigram 的查询需单独测试。
- [PostgreSQL CREATE INDEX](https://www.postgresql.org/docs/16/sql-createindex.html)：大表在线索引的锁、事务与失败恢复约束。
- [restic 分块和 pack 设计](https://restic.readthedocs.io/en/stable/100_references.html#backups-and-deduplication)：P3 的设计参考，不代表复用其仓库格式、参数或实现全部备份功能。
