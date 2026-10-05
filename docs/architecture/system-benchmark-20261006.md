# KYNXA 系统基准与对等指标（2026-10-06）

本轮实际运行公开 SciFact 全量检索对照，以及固定 DeepSeek 的 KYNXA／真实 Pi SDK 执行循环对照。检索排名、任务合同、证据可见性和实际执行分别计分，不合成含义不清的总分。没有提交、推送、停止或重启日常应用／网关。

Retrieval, strict agent contracts, visible evidence and real effects are evaluated separately. Published reference scores are not local runs or a full-product leaderboard.

## 同模型 Agent 对照

使用已授权测试连接的 `deepseek-flash`、相同资料／权限／默认供应商采样。12 类预冻结合成任务各重复两次，次轮反转执行顺序，共 48 次任务、123 次真实模型请求。每任务限 12 请求／24 工具／98,304 生成 token／120 秒，单次输出 8,192，连接窗口 32,768。共同生产准备保留 3,277 安全余量，实际输入预算 21,299；Pi 每任务记录的 29,491 是有效输入＋输出窗，不是不同预算。未修改正式连接。

KYNXA 使用生产 `ModelRuntime.replyStream`。对照为真实 `@earendil-works/pi-agent-core`／`pi-ai` **1.0.3** 原生循环，MIT 固定提交 `d78dc83d633229d12f8b79631384c4c2717c399f`，锁 SHA-256 `07ffd71cdd16f63f3ed182996a64fd41eb4b81dca3a334b7ef972194be677861`。两边共享 KYNXA 的初始系统提示、RAG、资料与真实 ToolService；Pi 自行维护循环与原生历史，**不是完整 Pi CLI／产品比较**。[上游执行源码](https://github.com/earendil-works/pi/blob/d78dc83d633229d12f8b79631384c4c2717c399f/packages/agent/src/agent.ts)

SDK 预检／导入在任何真实调用前完成，不读取个人 Pi 设置。每任务使用独立合成 Work/Data，评分器与隐藏 gold 不可由模型读取。两边相同固定 E5 q8、无神经重排；网页／浏览器／终端／桌面／远端 MCP 关闭。准备成本单列。

| 本轮全部成本，含合同失败 | KYNXA | Pi SDK 对照 |
|---|---:|---:|
| 严格合同通过 | **19/24，79.17%** | **17/24，70.83%** |
| 正常完成，无模型阶段中断 | 24/24 | 24/24 |
| 中英短问候，无工具／证据 | 4/4 | 4/4 |
| 文件复制／冲突安全修改／实际读回 | 4/4 | 4/4 |
| 失败后按依赖恢复流程 | 2/2 | 1/2 |
| 最终纯 JSON 合同 | 19/20 | 18/20 |
| 最终字段精确匹配 | 15/20 | 14/20 |
| 最终来源数组可解析且已观察 | 19/20 | 18/20 |
| 标准支持片段实际可见 | 20/20 | 20/20 |
| 模型调用／工具活动 | 60／49 | 63／55 |
| 输入 tokens，已含缓存命中 | 205,799 | 219,881 |
| 输出 tokens | 7,284 | 8,533 |
| 总 tokens | 213,083 | 228,414 |
| cache-hit，输入的子集 | 116,608 | 130,048 |
| 任务执行合计 | 59,350 ms | 63,147 ms |
| 资料／实例准备合计 | 26,513 ms | 24,164 ms |
| 两项合计 | 85,863 ms | 87,311 ms |
| 任务 P50／P95 | 2,639／3,953 ms | 2,989／3,910 ms |
| 任务首文本 P50／P95 | 2,518／3,561 ms | 2,608／3,741 ms |

首文本包含检索准备及前序纯工具阶段，不是最后某个 HTTP 的 TTFT。独立建库准备不等于真实聊天每次都建库。123 请求全部有 input/output usage，cache-hit 不重复相加；cache-write 未报告，`knownCacheWriteTokens=0` 只表示已知值合计，不能证明零写入。没有核对账单费率，不虚构金额。

KYNXA 本次输入少约 6.4%、总 token 少约 6.7%、执行合计少约 6.0%，加准备后少约 1.7%，P95 略高。供应商缓存、网络与规划有波动；12 类问题的重复不是 48 个独立抽样问题，不宣称普遍或显著优于 Pi，也不是新模块的单项因果消融。

### 失败诊断

`agent-system-v2`／`structured-facts-and-effects-v1` 首次推理前冻结；19/24 与 17/24 未改。独立 [人工诊断](../../artifacts/verification/agent-performance-benchmark/system-v2-1791225567470-9b36f551/semantic-review.json) 审核全部 12 个失败，非互斥类型：围栏 7、附加正文 2、sources 对象数组 3、Project 前缀 5、Sensor 前缀 2、遗漏 UTC 2、条件恢复依赖违规 1。

前缀与严格 ID 不符，但提示没有明确约定别名，是评分解释局限，不是编造实体。Pi 两次省 UTC 属于事实限定不完整。Pi 第二次恢复把 missing/fallback 同轮并行，没有等缺失再选择备用文件，答案正确而依赖流程失败。四次续问均保留 IRIS=Nora Vale、16:25 UTC，首轮两项目角色与窗口也正确；严格续问合同两边均 0/2，不能解释为记忆遗忘。

失败答案未见人物、日期或传感器数值与资料直接矛盾；人工诊断不另算满分或重评分。支持片段可见不等于所有生成句都正确。下一版若调整别名／schema，须新冻结版本并重新配对。

123 个实际请求的调用／返回均无孤立或缺对，摘要捕获的句柄长度只有 29 字符，出现 KYNXA 72 次、Pi 69 次，不是不同来源数。模型实际用短引用 `knowledge.read` **8／6 次，全部成功**，[独立回源核验](../../artifacts/verification/agent-performance-benchmark/system-v2-1791225567470-9b36f551/independent-shortref-call-review-20261006.json)。此前没有保存参数的旧 20 次烟测不能追溯宣称同一行为。

## SciFact 全量检索对照

全部 **300 test 查询、5,183 文档、22,858 分块**；官方 corpus/query/qrels 哈希核验，同原文／金标／document top-10。按实际返回顺序去重文档，线性 TREC gain，二值相关性。按 [BEIR 协议](https://github.com/beir-cellar/beir/blob/main/beir/retrieval/evaluation.py) 排除同 ID，本数据实际排除数为零；不使用金标选择块或调权重。

| 同机方法 | nDCG@10 | Recall@10 | MRR@10 | MAP@10 | 新测 P50／P95 |
|---|---:|---:|---:|---:|---:|
| 文档 BM25 对照 | 66.95% | 79.89% | 63.54% | 62.26% | 111.5／138.5 ms |
| KYNXA 分块词法 | 63.82% | 76.19% | 60.77% | 59.34% | 115.6／135.4 ms |
| 同索引向量检索 | 64.25% | 78.36% | 60.90% | 59.08% | 298.9／337.3 ms |
| KYNXA RRF 混合 | **67.57%** | **82.20%** | **63.97%** | **62.27%** | 410.2／487.4 ms |
| 历史实际 BGE 排序 | 66.40% | 83.64% | 61.76% | 60.34% | 本轮只复用排序 |

文档 BM25 为 SQLite FTS5 unicode61、title/text 等权、OR 查询、无 stemming，不是官方 Anserini 复现。计时包含最匹配既有块提取，不是纯 FTS 延迟。语义方法用实际固定 E5、新查询推理与生产索引 40 分块上限，都计查询嵌入；未重算全文向量。时延不含数据验证、复制、预热、会话授权快照、最终多预算投影或模型生成，不能当完整回答耗时。

Hybrid−同机文档 BM25：nDCG **+0.620 pp**，配对 bootstrap 95% CI **[-1.995,+3.270]**；Recall **+2.306 pp**，CI **[-1.000,+5.667]**，均跨零，不能称显著领先。相比历史原始质量基线，Recall **+4.017 pp**，CI **[+1.083,+7.100]**；与此前同优化索引逐查询排名／六指标完全一致。4000 次 bootstrap 按 300 查询，部分共享论文、未做聚类区间，不外推其他领域。历史 BGE 相比 Hybrid nDCG−1.164 pp、Recall+1.444 pp，两区间跨零；继续可选，不混入旧 native 时延。

### 最终证据预算覆盖

真实短句柄选择／投影 helper，同样 48 候选、最多六块、10,000 字符及回读提示；token 是仓库启发式估算。这是所选块涉及相关文档的覆盖，不是答案引用忠实度或 provider 实际输入。

| 方法 | 2,048 tokens | 4,096 tokens | 8,192 tokens |
|---|---:|---:|---:|
| 文档 BM25 | 69.93% | 73.21% | 73.21% |
| 分块词法 | 62.97% | 67.19% | 67.19% |
| 向量检索 | 64.95% | 69.39% | 69.39% |
| Hybrid | 70.73% | 72.34% | 72.34% |
| 历史 NN 排序 | 69.43% | 73.66% | 73.66% |

此样本 4K 已装六块，8K 不增加覆盖，不外推更长任务。24,549 句柄逐条核验 SQLite 源／分块哈希，独立重算 40,730 个指标／聚合／分位值。总时长 299.1 秒、CPU 316.2 秒、峰值 RSS 约 1.09 GiB，包含整个 FTS／E5／索引评测进程，不是应用空闲或单方法内存；源索引 122,822,656 字节。未停用户／OS 工作，检索与实际 API 阶段错开，机器不保证绝对独占。

同一完整 SciFact 的作者报告 nDCG@10：Anserini BM25 **66.5%**、multilingual E5-small **67.7%**，KYNXA **67.57%** 数值接近。量化、分块、输入与排序不同，未复现作者系统、未证明领先。[BEIR v4 Table 2](https://arxiv.org/html/2104.08663v4)、[mE5 2024 Table 7](https://arxiv.org/html/2402.05672v1)

近年公开 Agent 协议和不可横比范围见 [原始参照](benchmark-reference-20261006.md)；本轮未执行完整 BrowseComp-Plus／Agent Retrieval Bench，不将旧 40/500 或五题烟测放入公开尺度。

## 复现与独立核验

[Agent 入口及依赖锁安装](../../tests/agent-performance-benchmark/README.md)、[公开检索入口](../../tests/retrieval-benchmark/README.md) 提供命令。最终统一运行两个评测目录的回归，**47/47 通过，0 失败、0 跳过**：[本轮日志](../../artifacts/verification/system-benchmark-tests-20261006.log)。包括新 Agent 隔离回归 19、旧评测器／HTTP 链路 17、公开指标／哈希／配对统计 8 和既有指标 3，不相加重跑次数。模块守卫与差异检查通过；本轮未改桌面，没有新视觉／构建验收，不借用历史 835 项网关记录。

原 [Agent results](../../artifacts/verification/agent-performance-benchmark/system-v2-1791225567470-9b36f551/results.json)、[冻结 manifest](../../artifacts/verification/agent-performance-benchmark/system-v2-1791225567470-9b36f551/manifest.json)、[Agent 独立核验](../../artifacts/verification/agent-performance-benchmark/system-v2-1791225567470-9b36f551/independent-agent-audit-20261006.json)、[检索报告](../../artifacts/verification/rag-benchmark/scifact-peer-q300-d5183-v1-GaOAOY/report.md)、[检索独立核验](../../artifacts/verification/rag-benchmark/scifact-peer-q300-d5183-v1-GaOAOY/independent-audit.json)、[配对区间](../../artifacts/verification/rag-benchmark/scifact-peer-q300-d5183-v1-GaOAOY/paired-comparison.json) 独立保存。48 条 JSONL 与结果一致，30 源码指纹、SDK 锁和原产物哈希匹配，无旧成绩覆盖。

下一步预声明实体 ID／别名，区分机器格式与语义事实；条件 fallback 依实际失败再执行。效率侧测按任务选择词法／Hybrid、缓存失效及大资料回读成本。200 条消息、长任务恢复、真实仓库修复、桌面／浏览器任务需独立状态基准，不能从本轮短任务推定可靠。
