# 公共基准参照与 Agent 评分合同审查（2026-10-06）

本文核对公开原始论文、作者仓库和既有脱敏评测产物，不是一次新的模型实验。未调用真实 API、运行网关或读取用户 Data；不修改旧成绩、评测器和生产实现。

This reference separates comparable retrieval outcomes from agent-task and context-budget diagnostics. Published scores are reported results, not KYNXA runs or a model leaderboard.

## 哪些数字可以同尺度比较

“同尺度”要求相同查询、语料、金标、输出单位、截点与汇总方式；不表示相同模型、实现或硬件，也不代表论文系统已被复现。SciFact 的 BEIR test 是原任务公开 dev 的 300 条英文科学声明，语料为全部 5,183 篇摘要，金标为文档级二值相关性。[BEIR v4，Table 1 与 Appendix D.9](https://arxiv.org/html/2104.08663v4)

| 分类 | 原始报告/指标 | 精确参照值 | 可比条件或不可横比原因 |
|---|---|---|---|
| 可直接同尺度：完整 SciFact 文档检索 | BEIR BM25，document nDCG@10 | **0.665** | 300 queries / 5,183 docs；Anserini，k1=0.9、b=0.4；title/text 分字段。与相同原金标的 KYNXA 文档排序可比较效果，不能称为 SQLite 同参数 BM25 复现。[2021 v4，Table 2 / §4](https://arxiv.org/html/2104.08663v4) |
| 可直接同尺度：完整 SciFact 文档检索 | multilingual E5 small/base/large/large-instruct，document nDCG@10 | **0.677 / 0.693 / 0.704 / 0.718** | 作者 2024 报告的 SciFact 行；与完整文档指标可并列。KYNXA 内置为转换后的 multilingual-small q8、384 字符分块、上下文输入和 RRF，不能把作者结果称为同模型/同输入复现。[2024-02-08，Appendix Table 7](https://arxiv.org/html/2402.05672v1) |
| 可直接同尺度：完整 SciFact 文档检索；模型不同 | 英文 E5-PT small/base/large；监督 E5 small/base/large，document nDCG@10 | PT：**0.685 / 0.737 / 0.723**；监督：**0.656 / 0.731 / 0.726** | 原论文首次公开 2022-12-07，此处固定 2024-02-22 v2 的 Table 1/2；它们是英文模型，不能代替 multilingual E5 的参照行。[E5 v2，Table 1/2](https://arxiv.org/html/2212.03533v2) |
| 仅参考不可横比 | KYNXA 40-query / 500-document 试测 nDCG@10 | 原 hybrid **0.80908** | 语料借助金标缩减，查询也非全量；不能对比公开完整 SciFact 的 0.665 或 0.677。[评测入口说明](../../tests/retrieval-benchmark/README.md) |
| 仅参考不可横比 | 六分块覆盖、MMR/预算选择后的 recall、chunkPrecision | 按独立结果报告 | 六分块不等于六篇文档；2,048 tokens 可能只容纳部分候选。文档相关性不能证明该块支持某句答案或引用忠实。不能用这些数替换 document@10。 |
| 仅参考不可横比 | BrowseComp-Plus 检索与 Agent 成功率 | 见下文 | 830 个复杂多跳问题、100,195 文档、不同 gold、工具/模型/裁判与预算；不能把其中 Agent accuracy 移为 KYNXA 软件性能。 |
| 仅参考不可横比 | Agent Retrieval Bench 文件检索、BCY 和交互代理轨迹 | 见下文 | 冻结源码仓库和工作流金标，文件级 context acquisition；不是 SciFact 文档检索或最终修补正确率。 |

完整 SciFact 评测应先将重复 chunk 按已返回顺序折叠为原 `beirDocumentId`，再计算 top-10 文档，并在全部 300 查询间平均。KYNXA 的候选池、RRF、max-chunk 诊断聚合方式均应登记；它们是系统的检索设计，不妨碍相同文档尺度，但不能改称作者算法。BEIR 使用 `pytrec_eval`；TREC `ndcg_cut` 的 gain 是 qrels 原值，当前 JS helper 用 `2^grade-1`。本数据只有 0/1，二者相同；未来 graded qrels 必须另核官方公式。[BEIR evaluation.py](https://github.com/beir-cellar/beir/blob/main/beir/retrieval/evaluation.py)、[NIST ndcg_cut 实现](https://github.com/usnistgov/trec_eval/blob/master/m_ndcg_cut.c)

本次只读核验已下载公共数据：5,183 corpus IDs 与 300 test query IDs 无交集，BEIR 默认 `ignore_identical_ids=true` 不影响本组。不得把未标注文档人工补为相关，也不得用测试金标选择 chunk、检索权重或候选截点。

既有完整产物 `artifacts/verification/rag-benchmark/scifact-q300-d5183-s20261005-contextual-2o5nd8/results.json` 的原始文档 nDCG@10 为 lexical **0.6381814771**、hybrid **0.6756574405**、dense max-chunk 诊断 **0.6424686257**。这是已归档的系统检索结果：hybrid 可与上述 0.665/0.677 并列，但不证明整体 Agent 优于论文系统，也不证明当今源码已重新实测。当前生产证据短投影、预算选择或重排另行运行时须保留独立 source fingerprint 与成绩，不能补造旧产物身份。

延迟、内存和费用只在固定本机、线程、冷热状态、并发、工具初态、模型/资产和预算内比较。作者 GPU/CPU、不同 corpus 或异模型成本不与本机延迟横比。公开 nDCG 的提升不能推导 Agent 成功率、毫秒或美元节省。

## 2025–2026 Agent 基准能借鉴什么

### BrowseComp-Plus：把检索和最终答案分开

首次公开 2025-08-08；正式 ACL 2026 版题名为 *A Fair and Disentangled Evaluation Benchmark for Deep Search Agents*。后者新增分析并调整表号；这里明确采用正式论文，不混用 2025 的 Table 5 与 2026 的 Table 3。[ACL 2026 原文](https://aclanthology.org/2026.acl-long.1023/)、[作者仓库](https://github.com/texttron/BrowseComp-Plus)

| 2026 原表 | 指标/条件 | 作者报告数值 | KYNXA 可采用的评测方法 |
|---|---|---|---|
| Table 2 / §4.3 | 原完整 query；分别评 evidence-document / answer-bearing gold-document；无 Agent | BM25 evidence nDCG@10 **1.6%**、R@5 **1.2%**；Qwen3-Embedding-8B **20.3% / 14.5%** | 单独保存一次检索排名和整个 Agent 轨迹；不能混算两个 recall。 |
| Table 1 | 固定 GPT-4.1、相同 tool-use prompt；每 search top-5、每文档前 512 tokens；GPT-4.1 judge | BM25：accuracy **14.58%** / search recall **16.42%** / calls **10.35**；Qwen3-Embedding-8B：**35.42% / 36.89% / 8.67** | 固定同一 LLM、工具/初态/预算，仅改变检索条件；同时记 task correctness、检索并集覆盖和实际 calls。 |
| Table 3 | 固定 GPT-4.1 与同一 Qwen3 retriever，增加 get-document；提示也更新 | accuracy **35.42% → 43.61%**；search **8.67 → 10.03**；get-doc **1.85**/query | 小预览加按缺口回读值得测；作者收益不能直接迁移为 KYNXA 章节/window 收益，读取和新增提示都需计成本。 |
| Table 9 / Table 10 | 全部 830 queries 的 API 成本；不同判分方法 | GPT-4.1 BM25 **$106.96**，dense **$89.81**；dense accuracy substring / GPT-4.1 / Qwen3-32B judge：**34.46 / 35.42 / 36.39%** | 按失败与成功累计全调用；裁判版本和匹配规则必须固定。作者费用非当前价格、也不含 KYNXA 本地建库/推理成本。 |

以上全部数值来自 [ACL 2026 PDF，Table 1/2/3/9/10](https://aclanthology.org/2026.acl-long.1023.pdf)。Recall 是整个轨迹检索到的 evidence 文档并集覆盖；Citation Recall 是最终引用的 evidence 覆盖，二者还都不等于“每个事实由引用支持”。其仓库当前默认评测使用 Qwen3-32B judge，与论文 GPT-4.1 judge不同，不能静默混分。[作者当前评测说明](https://github.com/texttron/BrowseComp-Plus#-evaluation)

本机低成本入口可先用固定小任务集合验证 KYNXA 对正式数据/工具的完整链路；公开 BrowseComp-Plus 要获得论文同尺度分数仍需全部原 corpus、原 830 queries 和指定裁判。Windows 运行作者框架涉及 Python/Java 21，部分脚本以 Bash 为入口；大 dense index 与付费裁判不是已有桌面包的零成本能力。下载官方预建索引可省建库，但调用官方 retriever 只检验 KYNXA Agent 编排，不能证明 KYNXA 本地 RAG 检索性能。抽样或缩减 corpus 必须改标签，不能沿用完整榜单分数。

### Agent Retrieval Bench：检索到文件不等于完成 Agent 任务

2026-07-27 v1：25 仓库、427 样本，其中 345 正例、50 natural no-gold、32 wrong-repository controls。四类正例为 code2test、comment2context、trace2code、edit2ripple；必须用对应 base commit 的原文件语料。[原论文](https://arxiv.org/html/2607.24882v1)、[作者代码/数据入口](https://github.com/eyuansu62/agent-retrieval-bench)

| 原表/定义 | 精确作者报告 | 可借鉴与界限 |
|---|---|---|
| Table 4，345 positive，sample weighted | BM25：MRR **0.1520** / R@20 **0.4452** / BCY@8k **0.2051**；RepoMap **0.2158 / 0.6333 / 0.3788** | 排序、找到文件与预算内见到文件分别报告，不把某个榜首替成 Agent 成功率。 |
| §3 / Table 4 | BCY 在固定 token packing 下按 gold file 计曝光；canonical threshold **1 content token**，预算 **8k** | BCY 不保证读到了真正证据段。KYNXA 六 chunk 或估算 2048-token 不能直接横比 canonical BCY；有界章节任务应另验 gold span 实际可见。 |
| Table 19，natural-only，repo-grouped 5-fold calibration | lexical selective success@20 **0.294**，always-return **0.499**；mixed 则 **0.496 / 0.461** | synthetic wrong-repo 与自然无答案不能混合后声称拒答改进；原 top-score 并非通用充分性阈值。 |

上述数据和公式见 [原论文 Table 4/19 与 §3、§8.7](https://arxiv.org/html/2607.24882v1)。BCY 使用 benchmark 的规范分词/packing，含 path header 与分隔成本；轨迹中的 PES 是上界代理指标，不是已节省工具调用的因果值。作者闭合工具实验不允许修改/测试代码，未报告 patch success。

本机可预先指定一个官方 subset 或一组仓库，保留其全部 base-commit 候选文件并输出 file rankings，做无生成检索诊断；这适合测当前项目范围、path/symbol 与 code 词法机制。它仍须按子集标签报告，不与全部 345 正例 Table 4 横比。完整 benchmark 的大量 snapshots/文件/分块也不应被误称为当前本机已运行。评 Agent 文件操作应继续用真正状态验收，而不是将 ARB file Recall 抄成软件任务成功率。

## v3 两个漏判与下一版预冻结合同

只读核对 `run-1791223504850-9d06bee6/results.json` 与同目录 `independent-review-20261006.json`：原 v3 为 off **8/10**、on **10/10**。两个 QA off 答案都含正确 ORION、2031-11-14、17:40 UTC、Mara Chen、实验但未确认、fixture 文件引用；冻结关系解析结果却为空。第一条用尾部语句限定“以上仅涉及 ORION”，第二条在 prose 导语指定 ORION，空行后的 reviewer bullet 没继承实体。人工审查可以称为这两条的格式漏判，不能偷偷改成新成功率，也不能据此证明其他输出全部正确。

建议下一版独立 protocol/fixture/verifier 版本，按下列合同在调用模型前冻结、登记 hash；不重评 v1/v3，不与其自由排版任务当同协议对照：

1. 最终答案预声明结构化 JSON：例如 `status`、`facts[{entity,date,timeUtc,reviewer,confirmed,citations}]`、`answerZh`；问题提示只给空 schema/字段格式，不提供 gold。未知字段用 null，冲突/无记录状态用预定 enum，不能编造值。
2. 人工 gold 以实体关系 tuple 为单位，来源 manifest 固定版本、hash、事实支持偏移与优先规则；明确是否允许额外正确事实、额外无关事实和重复项，不能见到答案后再调整。
3. `schemaValid`、`factsCorrect`、`observedCitationSupport`、`finalStateCorrect` 分开。文件名出现不够：对每个 gold 事实确认正式观察里确实见到同版本支持片段；无证据任务不因检索到邻居就算有依据。需要人工裁判的开放域内容另报，不能伪装为纯函数严格证明。
4. 多文档/跨语言、最新版本与冲突、无答案、长章节/跨标题、续问和实际 read→write→read 为独立任务族；固定初始 Data、工具策略、模型/采样参数、预算与超时。RAG off/on 仍允许相同已授权文件读取，比较检索机制的总成本和失败行为。
5. 成功、事实覆盖、引用支持、工具/模型调用、全部 input/output/cache tokens、setup/执行时间、失败资源成本逐任务及总计报告；缺 usage 为 unknown。格式错误单独标记，不能与错误事实混称幻觉，也不能省略失败耗时来制造加速。

这些是 KYNXA 内部系统任务合同建议。原五题属于烟测；增加合成题也不会自动变成公开 Agent benchmark。两次重复可发现波动，不能支持统计显著或广泛能力结论；公开任务应保留出处、抽样规则与完整候选 corpus 身份。
