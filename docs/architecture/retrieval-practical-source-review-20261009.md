# 检索实用源码审查：原查询、跨语言召回与有限补查

审查日期：2026-10-09。本轮只审查作者源码与当前 KYNXA 合同，没有安装或运行以下 Python 框架，没有调用模型 API，没有改生产实现。固定源码与本地文件摘要保存在 `/tmp/kynxa-retrieval-research`；临时目录不随仓库交付，因此以下固定提交链接是正式复核入口。

目前优先验证已有多语词法/向量联合召回，再按缺失证据补一条查询。暂不引入每次默认四路改写、全问题 step-back 或额外的隐藏判断模型。词法提示继续作为候选与排序信号，不能重新变成领域资格或授权条件。

## 实测问题与适用范围

冻结的 40 题诊断使用 57 条短来源（46 条授权、11 条不授权），真实解析器和 Data 索引，固定 Top 5、每通道 16 候选，只有词法检索。34 道可回答题当前任一正确来源覆盖 33/34、全部所需来源覆盖 32/34；6 道无答案题仍返回弱相关来源，不能据此回答。失败与部分失败均保留：

- `volunteer-function`：中文“志愿者组织的 function 怎样帮助没有熟人的居民？”没有召回英语志愿者资料；词面 `function` 带来代码候选。允许跨域只解决候选被错误排除的问题，不能产生不存在的跨语言词面匹配。
- `trial-cancellation`：“仓库的 abort function 能不能同时保证参加者 withdrawal 合规？”找到代码，但需要的同意退出协议缺失。已有一条正确来源不等于复合问题的证据完整。

最终稳定时延实验有完整旧 Data/旧 builder、旧 builder/当前 Data、当前 builder/当前 Data 三组。同题同来源、预热后每题 10 次、随机配对顺序；当前完整 Data 路径的 plan/search/verify/read 均值 5.480 ms，旧版 4.738 ms，差值 +0.742 ms（均值之比 +15.7%）；paired 差值中位数 +0.519 ms、P95 +3.742 ms。搜索均值增加约 +0.700 ms。当前平均返回 4.8 项，旧版 4.65 项；作用域、原文、引用核验违例均为 0。

这些数字没有语义模型推理、重排、主模型 API、完整 Coordinator 或大索引成本。其用途是证明扩大候选可见性有小型词法成本，不能推导出完整 Agent 的绝对耗时或跨语言召回率。详细实验见 [扩展验证](ambiguity-expanded-validation-20261009.md) 与 ignored artifacts 中的 `retrieval-expanded-holdout-2026-10-09-postfix*.json`、`retrieval-holdout-latency-2026-10-09-final*.json`。标签冻结摘要为 `7b6b4a1549746e01d887bce6862d013d5ee917c5d39c9ebad9ab419897258524`，最终测量源码摘要为 `ea5e9127af08ba872651d85c2380213a41101f3d750e491631de25295aea52c9`。

## 固定作者来源与已读范围

| 来源 | 固定版本与实际读取 | 结论边界 |
| --- | --- | --- |
| LlamaIndex | 官方 `run-llama/llama_index`，提交 `6dd2f3cdd9ce7ff927ea8d46ccee08875bb00f95`；QueryFusionRetriever、HyDE/StepDecompose transforms、step-back 官方 notebook 的实现与示例 | 源码审查，不是本项目复现实验；notebook 不是成熟度或完整安全合同证明 |
| Haystack | 官方 `deepset-ai/haystack`，提交 `5fa8cff71d665ca9d179bd222965adaaefca0db7`；DocumentJoiner、RRF helper、BM25/dense retrievers、MultiQueryEmbeddingRetriever、LLMRanker | 查清了默认行为与边界；未读取或声称本轮跑过已经迁出的 SentenceTransformersSimilarityRanker |
| Multilingual E5 small | 作者 `intfloat/multilingual-e5-small` 的 [固定模型卡](https://huggingface.co/intfloat/multilingual-e5-small/raw/614241f622f53c4eeff9890bdc4f31cfecc418b3/README.md)，即当前项目清单固定的作者卡版本 | 读取使用说明、训练/评估描述与限制；没有本轮跑模型，也没有据模型卡声称 q8 在此语料一定改善 |

### LlamaIndex：保留原查询是有益设计，默认多查询仍需约束

[QueryFusionRetriever](https://github.com/run-llama/llama_index/blob/6dd2f3cdd9ce7ff927ea8d46ccee08875bb00f95/llama-index-core/llama_index/core/retrievers/fusion_retriever.py#L34)默认 `num_queries=4`、`use_async=True`、`mode=SIMPLE`。RRF 必须显式选择，不能把“用 QueryFusion”写成“自动采用 RRF”。

[_retrieve/_aretrieve](https://github.com/run-llama/llama_index/blob/6dd2f3cdd9ce7ff927ea8d46ccee08875bb00f95/llama-index-core/llama_index/core/retrievers/fusion_retriever.py#L276)先放入原始 query bundle，再追加 LLM 生成的最多三个查询。[_get_queries](https://github.com/run-llama/llama_index/blob/6dd2f3cdd9ce7ff927ea8d46ccee08875bb00f95/llama-index-core/llama_index/core/retrievers/fusion_retriever.py#L83)按行拆分并截数量，本类没有核对新增查询是否保留原实体、否定、时间与范围。

[_run_async_queries](https://github.com/run-llama/llama_index/blob/6dd2f3cdd9ce7ff927ea8d46ccee08875bb00f95/llama-index-core/llama_index/core/retrievers/fusion_retriever.py#L249)发出查询数 × retriever 数的任务并 `asyncio.gather`。这是并发调用，本类没有总并发上限，也不是一次模型批推理。默认四查询接双检索器会形成八次分支检索，外加一次查询生成；不能把异步写成免成本。

[RRF 实现](https://github.com/run-llama/llama_index/blob/6dd2f3cdd9ce7ff927ea8d46ccee08875bb00f95/llama-index-core/llama_index/core/retrievers/fusion_retriever.py#L113)按 node hash 合并，取相对名次之和。其源码零基名次使用 `1/(60+rank)`，与 KYNXA/Haystack 首名次 `1/61` 有细微约定差异；不能跨实现直接比较绝对分数。该分数不是事实支持概率，也不能充当资料足够的阈值。

[HyDE](https://github.com/run-llama/llama_index/blob/6dd2f3cdd9ce7ff927ea8d46ccee08875bb00f95/llama-index-core/llama_index/core/indices/query/query_transform/base.py#L96)先生成假想文档，默认保留原查询并添加用于 embedding 的文本。这可以提供召回表示，但假想文档不是证据，不能生成项目引用或被当成已知事实。它增加模型调用，目前不应先于已有真实多语 embedding 的对照实验。

[step-back 官方 notebook](https://github.com/run-llama/llama_index/blob/6dd2f3cdd9ce7ff927ea8d46ccee08875bb00f95/docs/examples/evaluation/step_back_argilla.ipynb)示例将具体日期内的问题改成更泛的问题，依次检索 step-back 与原查询后综合回答。原问题在最终提示仍保留，但泛化会扩大时间范围；`async` 包装内仍有同步 `llm.complete`，两次 `aretrieve` 也是依次等待。该示例支持“泛化分支可以辅助原问题”，没有证明它总会保持实体/否定/时间、减少等待或修复授权问题。

### Haystack：融合可复用，授权范围必须外置且不可覆盖

[DocumentJoiner](https://github.com/deepset-ai/haystack/blob/5fa8cff71d665ca9d179bd222965adaaefca0db7/haystack/components/joiners/document_joiner.py#L88)默认 `CONCATENATE`，重复 doc ID 取最高分；weighted merge 与 RRF 都要选择对应模式。直接拼接 BM25 与向量原分数不保证量纲可比。

其 [RRF helper](https://github.com/deepset-ai/haystack/blob/5fa8cff71d665ca9d179bd222965adaaefca0db7/haystack/utils/misc.py#L159)以 doc ID 去重、归一化分支权重，零基名次加 61，最后缩放总分。这适合作为不同通道的排名融合，没有判断是否支持当前结论，也没有 KYNXA 的来源版本、内容 hash、授权 scope 或 `rag1` 引用合同。

[BM25](https://github.com/deepset-ai/haystack/blob/5fa8cff71d665ca9d179bd222965adaaefca0db7/haystack/components/retrievers/in_memory/bm25_retriever.py#L147)与 [dense](https://github.com/deepset-ai/haystack/blob/5fa8cff71d665ca9d179bd222965adaaefca0db7/haystack/components/retrievers/in_memory/embedding_retriever.py#L167)都在 document store 查询前传递 filters 和 Top K。这比先全库取 Top K 再过滤更符合授权范围内召回。但两者初始化默认 `filter_policy=REPLACE`，调用时的 runtime filter 可以替换初始化 filter；KYNXA 的授权范围不能只放在一个可被调用参数替换的初始化 filter 中。每个分支都必须由程序加上本轮真实 scope，读回仍核验版本。

[MultiQueryEmbeddingRetriever](https://github.com/deepset-ai/haystack/blob/5fa8cff71d665ca9d179bd222965adaaefca0db7/haystack/components/retrievers/multi_query_embedding_retriever.py#L127)提供默认三 worker 的线程并发，异步版有 semaphore 与任务取消 helper。它对每条 query 各发一次 embedding 和 retrieval，再按 doc ID 去重、原分数排序，本类并不自动 RRF，也不自动加入原查询。使用者必须把原查询放入 queries，并给所有分支同一不可扩张的授权条件。限制并发有用，但每条单发仍不等于真正的张量/API batch。

[LLMRanker](https://github.com/deepset-ai/haystack/blob/5fa8cff71d665ca9d179bd222965adaaefca0db7/haystack/components/rankers/llm_ranker.py#L264)将原问题和去重候选交给一个额外 generator，解析返回的排序编号。失败时返回未排序的去重输入，源码 fallback 路径没有最终 Top K 截断。借用此模式时需要重新应用本项目候选/上下文预算、canonical 引用、来源有效性与取消合同。KYNXA 的主模型已经要选择候选；常规任务不必再默认增加一次独立 LLM judge 回合。

### 多语语义模型：已有基础设施，跨语言收益需要实际验证

E5 作者模型卡要求非英语同样加 `query: ` 或 `passage: `，masked mean pooling 后归一化，最长 512 token。支持 100 种语言不代表每种语言等效，作者明确低资源语言可能下降。其 FAQ 说明 cosine 通常集中于 0.7–1，排序相对关系比绝对值重要；不能用“相似度 0.8”推断答案可靠。

KYNXA [embedding-profile](../../apps/model-gateway/models/retrieval/embedding-profile.mjs)固定 Xenova 转换版 `761b726dd34fb83930e26aab4e9ac3899aa1fa78:q8`，384 维、上述双前缀与 512 token。作者 PyTorch 模型卡分数不能直接当作此 q8 转换、Windows 原生后端或本项目任务的结果。CPU 与 GPU 的模型空间也明确分开，不能把不同空间向量混用。

## 当前实现与实际改造差距

| 需求 | KYNXA 当前已有 | 本轮仍需验证或设计的差距 |
| --- | --- | --- |
| 词法 + 向量联合 | [Data search](../../apps/model-gateway/data/retrieval/index-worker.mjs)有 lexical/vector/symbol/path 通道与加权 RRF；每通道先受 scope/domain 合同约束；弱 `preferredDomain` 只影响排序 | 本次 40 题没有运行向量。需确认真实模型、完整向量覆盖和实际后端，而不是再实现一套同类融合 |
| 原话与派生查询 | 原 query 传给检索/重排，原文切片与引用不因 preference 改写；已有 bounded 技术术语桥 | 如增加派生分支，应显式记录原话、具体缺口、增加的术语与依据；不覆盖原话，不将推测实体变成硬条件 |
| 候选去重与多样性 | [candidate-selection](../../apps/model-gateway/orchestration/retrieval/candidate-selection.mjs)有相同/重叠切片去重、软文件多样性、不同证据片段保留、目标优先 | 调整顺序和预算要比较全所需事实覆盖，不能为了少读来源而合并掉同文件第二条必要证据 |
| 可选重排 | [Coordinator](../../apps/model-gateway/orchestration/retrieval/coordinator.mjs)按任务/ready 状态决定本地重排；失败降级；只接受原候选的 canonical sourceRef | 重排不能恢复完全漏召回的 volunteer 来源；需度量候选覆盖后才选择是否重排、是否采用主模型选择 |
| 批量与并发 | document embedding 有预算内真实批处理；Coordinator 与 Index worker 有序执行 | [EmbeddingService](../../apps/model-gateway/models/retrieval/embedding-service.mjs)仅公开单 `embedQuery`。未来若需要查询批量，应新增受验证的 query-prefix 批接口；不能误用 `embedDocuments`。Promise.all 也不会把同一串行 Index worker 变成数据库并行 |
| 缓存与新鲜度 | acquisition cache 绑定查询、scope/snapshot、设置、intent、来源身份；命中仍 fresh/assertCurrent；空或失败结果不当作缺口已解决 | 跨请求 query-vector 缓存不是当前公开合同；若加入应绑定模型/空间/前缀/规范化查询，并受资源与隐私上下文约束，结果缓存仍要逐项当前授权/版本核验 |
| 有界停止与降级 | [EvidenceAcquisition](../../apps/model-gateway/orchestration/retrieval/evidence-acquisition.mjs)有总搜索、每缺口、无进展限制；不可用 embedding/rerank 有显式降级与取消/资源合同 | 派生策略必须共用总墙钟/模型调用/token 预算，失败不能启动无限改词；额外冷却断路只有实际重复故障数据证明确有必要才加 |

正式 Coordinator 默认 lookup 候选每通道 48、融合 64、最终 8；complex/research 候选至少 64，默认 96、融合默认 128、最终 18，受用户指定限制和上下文预算约束。诊断实验的 Top 5/16 不能代替这些真实生产预算下的效果与成本。

## 建议的三个优化顺序

1. **先测现有真实多语 hybrid，保持其余机制不变。** 用已冻结 40 题及新的未参与开发跨语言留出，分别跑词法、向量、联合；固定模型版本、scope、Top K、候选与 token 总预算。确认资产哈希、query/document 前缀、向量空间、索引覆盖和 ready/cold 状态。分别观察 `volunteer-function` 候选是否出现与 `trial-cancellation` 全部事实是否齐全，不预先承诺修复。这一步主要是评测，可能只需运行现有路径。

2. **仅对回读后明确缺口，保留原查询并最多追加一条派生检索。** 主模型在已有工具回合表达未解决的中性子问题，不默认新增 query optimizer 调用。volunteer 可把 volunteer group/social function 作为待验证候选；trial 应独立补查 participant consent/withdrawal，而不是继续只查 abort。派生内容保存修改依据与被保护的实体、否定、数字、时间、范围；通道在同一 scope 内融合，来源使用已有 canonical 合同。一次补查后无新证据则停止或改读已知来源；全局仍有调用、token、墙钟预算。先试一条，数据支持后才考虑更多。未证实候选不取得权限或硬领域资格。

3. **候选覆盖成立后，再优化重排、批量与缓存。** 比较已有去重/多样性 + 主模型选择与可选本地重排，保留不同文件和同文件不同必要事实。若多查询确实常用，新增有界 query embedding batch、request-local 去重/缓存；减少重复模型激活比纯粹 Promise.all 更有实证意义。错误或超时直接返回已有授权候选和明确覆盖状态，停用不可用可选分支，不能让所有请求先等待冷启动。较少的候选/回读只有在事实覆盖和任务完成保持时才算优化。

不采用自然语言关键词硬路由；不把 RRF、相似度或模型口头置信度当成充分证据；不通过“只展示一种工具”掩盖应可见的来源；不拿宽泛 step-back 查询的资料回答原题中的具体时间/否定条件。

## 下一轮必要评测

新实验应先冻结来源、所有必要事实、负例与预算，再运行，保持失败记录。至少包含中英混用/中文查询英语资料、品牌/符号同名、复合证据、反事实/否定、换题/纠正、无答案和只存在于非授权 scope 的答案。40 题是诊断集，优化后需增加未参与设计的人类标注留出；正式规模建议覆盖 1 万及 10 万 chunk、有同源重复、长来源和不同 scope 密度，不凭短文档扩份保证结论。

每次只变一个机制并保持总预算，分别记录：

- 候选 Recall@K、全部必需事实 Recall@K、来源多样性、有效回读、主模型最终选用来源、答案支持度/无证据回答率。无答案题的有候选率另记，不能混入正确召回。
- 原 query 保留、派生实体/否定/时间/范围变更、非授权来源、旧版引用拒绝、过期结果与取消后余计算。
- plan、embedding、各通道检索、fusion/selection、rerank、verify/read、主模型 API、端到端首个有用结果与最终结果的 P50/P95/均值。
- 冷启动、warm、缓存命中/未命中、CPU/GPU 原生后端、队列并发、内存峰值、真实 API 调用数/token/费用；失败、超时与模型不可用时的退化。

真实 API 评测要固定主模型/版本/温度与调用预算，对照“现有策略”“按需缺口补查”“有条件重排”，不能把检索器 Top 5 改善直接写成模型任务正确率。当前没有做上述语义与 API 实验；+0.742 ms 只作为已完成小型词法测试的局部成本基线。
