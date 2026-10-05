# 本地检索数据层

由 E 维护，正式数据根取自 `ConversationStore.root`。本模块不导入模型、工具或编排域，不读取任意工作目录。编排层与既有工具策略先授权并解析来源，再传入正文、稳定身份和范围。

## 入口

- `settings.mjs`：全局与项目检索设置，正式目录保护、文件队列、revision CAS、白名单校验。项目覆盖以 `local/web/cache: null` 清除；挂载索引默认关闭，绑定版本只由后端递增。
- `index.mjs`：`RetrievalIndex` 的异步端口和资源所有者；`chunkSource` 导出给嵌入服务的调用方。
- `index-worker.mjs`：单个受控 worker 内的 SQLite FTS5、持久来源与分块、sqlite-vec 精确距离、RRF、代次发布和 WAL 生命周期。
- `retrieval-text.mjs`：确定性中文单字/二元词、英文与驼峰/下划线标识符、标题/段落/可识别代码边界分块。文档保留词频，查询词单独去重。默认 384 字符，保留原文字符和行号定位；这不是完整 AST 分析。
- `embeddingTextForChunk(source, chunk)` 与 `EMBEDDING_TEXT_VERSION` 从 `retrieval-text.mjs` 导出：嵌入输入使用真实标题、相对路径和当前 Markdown 小节，上下文最多 128 字符、总输入默认 512 字符，优先保留原始分块。字符截断保留 Unicode 字符；实际 token 上限仍由嵌入模型 tokenizer 校验。该输入投影不修改引用正文、哈希、偏移或 `CHUNKER_VERSION`，调用方将上下文版本纳入模型版本匹配。
- `retrieval-contracts.mjs`：有界输入、哈希、范围与版本化引用校验。

## 持久位置与身份

`Index/retrieval.sqlite` 是可重建的派生索引，与侧栏的 `Index/search.sqlite` 分开。`Retrieval/settings.json` 与 `Projects/<项目 ID>/retrieval.json` 保存用户配置。`Retrieval/source-identities.json` 保存来源身份、范围、逻辑定位及撤销状态，不保存第二份正式聊天。

`sourceRef` 包含来源与分块版本，不因索引重建或 Data 移动改变；资料已删除或版本变化时读取明确拒绝，不绑定到同名新资料。来源允许范围仍由请求后端提供，引用不是额外权限。正式工具回执/消息由既有存储保存，不能只依赖该索引保留已交付引用。

`listSources` 只列允许范围内的来源元信息，供协调层对账。`removeSource` 默认写永久 tombstone，使迟到任务不能复活已撤销的稳定 ID；挂载变化或无效派生来源对账使用 `permanent:false`，允许有效来源重建。`invalidateScope` 只使范围索引失效，不删除原文或清除既有永久撤销。`indexSnapshotId` 由数据库 epoch 与 generation 组成，删库重建也不会与旧缓存键碰撞。

## 检索和降级

每次搜索、读取和删除都要求非空已授权 `scopeKeys`。词法查询按绑定参数预过滤；向量通道先物化允许范围、同嵌入 profile/模型版本/维度的候选，再计算距离，不做全库 Top-K 后过滤。初版使用 sqlite-vec 标量距离的有界精确扫描，不宣称 ANN；范围内向量超过 50,000 块时词法降级并返回原因。

两通道各最多 40 候选，RRF 合并；最终最多 60 块。原生扩展缺失时词法仍可运行，诊断明确标识。向量支持按 chunkIndex 对齐的 null 空项；嵌入模型过长、失败或不可用的块不能用伪造向量补齐。调用方同时传入 `embeddingProfileId` 与 `embeddingModelVersion`，防止稳定 ID 换权重后复用旧向量。

FTS 匹配先物化 `rowid` 与 BM25 分数，再在已授权范围内按精确正文短语、BM25、稳定分块 ID 排序取 40 个候选；正文和宽元信息仅在截点后回取。向量排序同样只物化已授权 ID 与向量，取候选后回取正文。不依赖 `ANALYZE`、数据库全库 Top-K 或每次查询写统计来获得该计划。`lexicalRank`、`vectorRank` 是从 1 开始的通道位置；`lexicalScore` 为原始 BM25 分数（越小越相关），`distance` 为余弦距离，均不是置信概率。

`SourceLibrary.readSource(sourceId, { scopeKeys, sourceRevision })` 在库队列内核对正式登记、范围、项目状态、版本和磁盘快照哈希，仅回读一份来源。已撤销、版本不符或快照异常返回 `null`；异常状态供资料列表展示，不能用缓存复活来源。登记损坏仍明确失败。

## 短证据引用与有界回读

`evidence-references.mjs` 将最终检索条目投影为固定 29 字符的 `ev1:<归档 UUID 的 22 字符编码>:<两位 base36 编号>`，编号为 1–60。`allocateEvidenceArchiveId()` 仅预分配 ID；`ToolResultStore.save(context, call, canonical, { id })` 在最终发布时原子保存，已有 ID 明确冲突，不能覆盖。短引用可从正式归档恢复，不依赖进程内别名表。草稿成本和最终编号由编排层协调；正式归档仍保存完整 `rag1`、来源/分块哈希、revision 和 scope。

`projectEvidenceSearchResult(result, archiveId)` 支持原始检索结果与含 `structuredContent.items` 的工具结果，按最终条目顺序编号，校验完整 canonical 元组后替换模型 `sourceRef`，省略重复身份长字段，保留正文、定位、评分与 assessment/acquisition/rerank 诊断。调用方先使用 `publicToolResult` 剥除私有 `_meta`；纯短引用 helper 不代替隐私视图。仅 `ToolResultStore.modelResult` 将 `knowledge.search` 历史结果短投影，`get`、`read` 与磁盘 canonical 完整视图不变。

`EvidenceReferenceStore({ conversationStore, resultStore }).resolve(context, shortRef, { scopeKeys, signal })` 必须找到当前聊天正式助手的 `RetrievalResultRef` 或相应 `ToolActivities` 回执，再校验归档 bytes/sha256、requestId/toolCallId/toolName 和当前工作范围。聊天重启仍可回查；跨聊天、移出原项目、归档状态、伪造编号或改写归档均拒绝。`resolveTrusted` 仅供刚发布、尚未登记日志的本请求内部调用，须提供完整回执与一致的 requestId，不得暴露到模型 schema。解析只返回身份/定位，不返回旧归档正文；调用方必须继续进行当前 index 与正式来源的版本、撤销、磁盘哈希校验。

`RetrievalIndex.readWindow({ sourceRef, scopeKeys, mode, anchorOffset, beforeCharacters, limit, signal })` 只接受完整 canonical 引用；`mode` 为 `window` 或 `section`，默认锚点为匹配分块起点，默认前文 384 字符、返回上限 4000，允许上限 2–16000 UTF-16 字符，前文范围 0–4096。原文/分块/登记不改变，偏移准确且不会拆开代理对。`source-window.mjs` 按实际 Markdown ATX/Setext 标题和层级寻找章节，忽略围栏代码标题；无标题时回退窗口。它是有界 Markdown 导航，不是通用文档结构解析器。

默认引用跨标题时不声称它属于单章：返回覆盖原分块的有界窗口和最多 16 个 `window.sections` 导航条目，标明 `spansSections`、`navigationTruncated`、`referenceRange` 与 `referenceRangeCovered`。预算过小时覆盖可能不完整，必须依据标记继续回读；显式 `anchorOffset` 精确选择该位置所在章节。正文与导航均为原文偏移，章节过长会裁剪并标记；实际模型 token 预算仍由编排层控制。旧 `read` 分页行为保留。

Short evidence references resolve through formal chat receipts and durable canonical archives; they grant no additional access. Window/section reads return bounded original text and exact offsets, with explicit navigation when a chunk crosses headings. Archived excerpts never replace current source freshness checks.

## 并发与迁移

来源发布在一个 SQLite 事务内更新正文、分块、向量和范围代次；批次最多 100 来源、8M 字符。过期 numeric sourceRevision 或 bindingRevision 被拒绝，已返回成功回执不因晚到取消而消失。

词法版本 `han-bigram-code-tf-v2` 启动时按每批 256 块迁移已知 v1 数据，仅更新 `lexical_text`、`tokenizer_version` 并触发 FTS 更新，推进对应范围快照以失效查询缓存。原始来源、稳定 ID、撤销登记、原文、分块哈希、偏移和向量全部保留；事务失败可在下次启动续迁，不删除索引文件或原始记录。

调用方在 Data 迁移前停止新增任务、等待既有操作，并执行 `close()`。worker 完成 WAL TRUNCATE checkpoint、关闭连接后退出；索引不能保持打开时仅复制 SQLite 主文件。未来 schema 与异常链接路径拒绝覆盖。正式 JSONL、记忆与原始文件不受清索引影响。

独立验证：`node --test apps/model-gateway/tests/retrieval-data-index.test.mjs apps/model-gateway/tests/retrieval-data-settings.test.mjs`。全部使用临时 Data 与合成来源，不读取用户日常数据或调用云端模型。
