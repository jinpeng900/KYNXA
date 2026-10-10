# Public retrieval evaluation / 公开检索评测

## 同尺度完整对照（2026-10-06）

`run-peer-scifact.mjs` 接受已完成的完整 300 查询／5,183 文档公开评测，复制关闭后的索引并核验每个原文、块哈希和偏移；不重算全文向量、不触碰原评测或用户 Data。实际运行文档 SQLite FTS5 BM25、生产 chunk lexical、同索引 40 块 vector-only 和 RRF hybrid，按官方 document@10 去重与同 ID 排除规则计算 nDCG／Recall／MRR／MAP／Precision／Hit。文档 BM25 有独立原文 FTS，不能改称官方 Anserini 同实现复现。

```powershell
node tests/retrieval-benchmark/run-peer-scifact.mjs --input <completed-full-scifact-run> --rerank off
node tests/retrieval-benchmark/run-peer-scifact.mjs --input <completed-full-scifact-run> --neural-input <completed-neural-run> --rerank cached
node tests/retrieval-benchmark/audit-peer-scifact.mjs --input <new-peer-run>
node tests/retrieval-benchmark/compare-peer-scifact.mjs --input <new-peer-run> --historical-optimized <optimized-run> --historical-legacy <legacy-run>
node --test tests/retrieval-benchmark/peer-scifact-metrics.test.mjs tests/retrieval-benchmark/peer-scifact-comparison.test.mjs
```

`--repeats 1..5` 可增加新查询计时，质量对照不把重复当新独立问题；默认使用公开缓存数据与已验证本地 E5，没有网络或模型 API 调用。`--rerank cached` 只复用历史真实 logits／排序质量，**不记录本轮 NN 延迟**；`live` 才是新的真实 BGE 推理，原资产必须存在。新目录中 `results.json`、原排序、时延、资源及报告保存，独立 audit 重算数值和关联哈希。

2K／4K／8K 预算用同短引用投影 helper、同 48 候选／六块／10,000 字符／回读提示，度量最终相关文档覆盖；不是 provider 计费 token 或生成答案忠实度。新计时包含各语义方法一次真实查询嵌入，文档 BM25 含最佳既有块提取；排除冷预热、资料验证、复制、会话授权、最终投影和生成。峰值内存属于整个评测进程，不等于某方法或应用空闲占用。

`compare-peer-scifact.mjs` 按唯一 queryId 做配对 bootstrap；有 repeats 时先平均，不扩大样本数。二值 SciFact 的 TREC 线性 gain 与旧指数 gain 一致；其他 graded 金标应核官方定义。作者报告、同机基线、独立核验与实际 DeepSeek Agent 配对结果见 [完整系统基准报告](../../docs/architecture/system-benchmark-20261006.md)。

This entry runs KYNXA's real SQLite lexical/hybrid index, fixed offline multilingual E5 model and production evidence selector against either a seeded SciFact sample or the complete test split/corpus. It is an evaluation entry owned by the integration lead, with Data/Models review; the runner does not modify production sources.

本入口用实际 SQLite 词法/混合检索、固定离线 multilingual E5 与生产证据选择器测试 SciFact 确定性样本或完整测试集。评测入口由 A 集成负责人协调，E/C 审查数据和模型使用；脚本不修改生产模块。

This is an Agent retrieval-component regression, not an Agent overall score or a ranking of chat models. It does not execute task planning, paid chat generation, browser actions, tool approvals, cancellation/recovery or a final answer. Those behaviours need separate observable end-to-end task tests.

这是 Agent 检索组件回归，不是 Agent 总分或聊天模型排行榜。它不执行任务规划、付费聊天生成、浏览器操作、工具审批、取消/恢复或最终回答；这些行为须另做可观察的端到端任务验收。

```powershell
node --test tests/retrieval-benchmark/metrics.test.mjs
node tests/retrieval-benchmark/run-scifact.mjs --queries 40 --documents 500 --seed 20261005 --repeats 3 --offline
node tests/retrieval-benchmark/run-scifact.mjs --full --seed 20261005 --repeats 1 --offline --rerank
node tests/retrieval-benchmark/run-rerank-existing.mjs --input artifacts/verification/rag-benchmark/COMPLETED_RUN_DIRECTORY
```

The script downloads the [official BEIR SciFact archive](https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip), checks its [published checksum](https://github.com/beir-cellar/beir/wiki/Datasets-available), preserves original bytes, and records SHA-256 hashes. Windows uses built-in PowerShell archive extraction; other systems require `unzip`. The local model must already be prepared with the existing build workflow. No Python, paid LLM, active gateway or user Data is used.

脚本下载官方 SciFact 压缩包，验证公开校验值，保留原始文件并登记 SHA-256。Windows 使用自带 PowerShell 解压，其他平台需要已有 `unzip`。离线模型须已按现有构建流程准备；不使用 Python、付费模型、运行中的网关或用户 Data。

Outputs go to ignored `artifacts/verification/rag-benchmark/`: archive cache, original selected documents, judgments, source mappings, ranked results, raw timings, results and independent temporary index. Every batch/query writes `progress.jsonl` immediately; successful query rankings are also appended immediately, so redirected PowerShell output cannot hide progress or discard an interrupted pass's completed queries. `--offline` forbids dataset downloads; `--model-root PATH` selects existing verified model assets. `--cache PATH` and `--output PATH` choose dedicated evaluation locations. No existing files are deleted.

产物放在已忽略的目录：压缩包缓存、所选原文、相关性标注、来源映射、排序、原始耗时、结果及独立临时索引。每批和每条查询立即写 `progress.jsonl`，已完成查询立即追加排序，避免 PowerShell 重定向隐藏进度或中断时丢失整份排序。`--offline` 禁止下载，`--model-root` 指向现有受验证模型；`--cache` 和 `--output` 可指定专用评测位置。不会删除已有文件。

`--full` requires all 300 test queries and all 5,183 documents. The original default remains the same 40 queries/500 documents/seed as the archived pilot. Query/document IDs, original text hashes and public qrels are recorded before retrieval. Compare a full run only with another full run under the same judgments/settings; the gold-assisted reduced-corpus pilot is not a full-corpus baseline. A complete run is a custom KYNXA retrieval evaluation, not an official BEIR leaderboard submission.

`--full` 固定使用全部 300 条测试查询和 5,183 篇文档；默认仍保持旧试测的 40/500 和随机种子。检索前登记 ID、原文哈希与公开金标。完整集只能与同设置的完整集比较；借助金标构造的缩减集不能充当完整集基线。完整执行仍是 KYNXA 自定义检索评测，不冒称官方榜单提交。

`--embedding-input contextual` (default) uses the actual versioned `embeddingTextForChunk` helper. `--embedding-input legacy` uses the original chunk body. Strict Float32 `.bin` caches have manifests keyed by original text hashes, offsets/chunk hashes, chunker/tokenizer/input versions and pinned E5 model/code/asset hashes. `--vector-cache PATH` changes the dedicated public evaluation cache. Corrupt or mismatched entries trigger real recomputation; cached vectors are never invented or substituted from qrels. Warm query latency still recomputes query embeddings and does not use the evaluation cache.

默认 `--embedding-input contextual` 使用生产的版本化上下文嵌入 helper；`legacy` 使用原始分块正文。Float32 `.bin` 缓存的清单严格包含原文哈希、偏移/分块哈希、分块/分词/输入版本和固定 E5 模型、推理代码、资产哈希。`--vector-cache` 可选择专用公开评测缓存。损坏或身份不符会真实重算，不伪造向量、不根据金标替换结果；热态查询耗时仍重新计算向量，不使用评测缓存。

`--baseline-root PATH` is optional and imports a previously frozen `index.mjs`/worker/text/contracts execution snapshot. It must be paired with `--embedding-input legacy` when reproducing the old quality settings. Snapshot hashes are reported separately. A faster baseline may replace only equivalent SQL after strict chunk-order/score equivalence checks against the original saved rankings and direct original-SQL probes; it must retain original tokenization, chunking and vectors. This option never silently captures or alters the current production code.

可选 `--baseline-root` 读取预先冻结的索引/worker/文本/合同执行快照；重现旧质量设置时搭配 `--embedding-input legacy`，并单独登记快照哈希。仅在原排序与原 SQL 探针逐分块顺序/分数严格相等后，可以使用等价提速 SQL，仍保留原分词、分块与向量。该选项不会静默抓取或修改生产代码。

The primary selected-evidence comparison keeps the same 2,048-token budget. Additional 4,096/6,144/8,192-token sweeps report budget sensitivity separately. A budget increase is not attributed to tokenization, embedding or reranker quality. The six-chunk limit is a maximum; a tight budget can select fewer chunks, and the actual count is reported.

主要证据选择对照保持相同 2,048-token 预算，另列 4,096/6,144/8,192 档位敏感性。扩大预算的改善不归因于分词、嵌入或重排质量；六个分块是上限，紧预算可能只选更少分块，报告实际数量。

Selected `chunkPrecision` uses document-level public qrels: a chunk from a relevant document is counted as relevant. It is a document-relevance proxy, not a sentence-span evidence, factuality or citation-faithfulness score. `repeatedDocumentSlots` counts additional chunks from the same document and does not claim their text is duplicated.

选择结果的 `chunkPrecision` 使用公开文档级金标：相关文档中的分块算相关。它仅是文档相关性代理指标，不是证据句、事实正确性或引用忠实度评分。`repeatedDocumentSlots` 表示同文档的额外分块位置，并不断言正文重复。

**The default reduced-corpus pilot is not an official full BEIR score.** Every positive document for the sampled queries is included; remaining documents are seeded unjudged distractors. Gold-assisted corpus reduction makes this pilot optimistic compared with full-corpus retrieval. Standard Recall/Hit/MRR/nDCG are macro-averaged over document rankings at 1/3/5/6/10. Document rankings collapse repeated chunks in returned order. Raw first-six-chunk coverage and the production budgeted/MMR-selected evidence are reported separately; neither silently replaces six chunks with six distinct documents. The selected projection reports its actual count, token budget and duplicate/source coverage. The dense-only cosine/max-chunk comparator scores every document but saves the first 60 ranks, and remains explicitly diagnostic.

**默认是抽样、缩减语料试测，不是正式完整 BEIR 分数。** 所选查询的全部正相关文档都保留，其他文档是确定性抽取的未标注干扰项；金标辅助缩减语料会使结果比全语料检索更乐观。标准指标按文档计算并在查询间平均；原前六分块与生产预算/MMR选择后的证据分开报告，不能将六个分块偷换为六篇文档。选择投影记录实际数量、token 预算、重复与来源覆盖。纯向量比较用真实向量逐文档精确 cosine/max-chunk 评分、保存前 60 名，仅作为诊断比较。

`--rerank` additionally runs the real offline BGE reranker over at most 20 deduplicated hybrid candidates, then applies the same selected-six token budget. `--reranker-root PATH` selects verified existing assets. Raw hybrid, neural reranking and final evidence metrics remain separate. Missing assets or inference failures fail this requested ablation; no lexical result or mocked score is mislabeled as neural reranking. Returned truncation counts and actual reranking latency are recorded.

`--rerank` 另用真实离线 BGE 重排最多 20 个去重后的混合候选，再应用同一六证据预算。`--reranker-root` 选择已验证资产。原始混合、神经重排和最终证据指标分别列出；资产缺失或推理失败会明确使该对照失败，不将词法结果或模拟分数冒充神经重排。登记返回的截断计数和实际重排耗时。

`run-rerank-existing.mjs` performs this ablation on an already completed production-index evaluation without repeating document embedding. It accepts only the public pinned dataset, verifies every hybrid chunk/order/score against the saved ranking, and writes a separate report. Legacy baseline snapshots are deliberately rejected to preserve their original tokenizer/index. The reranker receives real original candidates and fixed local assets; query vectors may reuse the same verified public cache.

`run-rerank-existing.mjs` 可复用已经完成的生产索引评测，单独重排而不重算文档向量。仅接收固定公开数据集，每条混合候选的分块、顺序、分数都与原排序严格核验，另存报告。旧基线快照明确拒绝，保留其原分词/索引。模型读取真实原候选及固定本地资产；查询向量可复用同一受核验的公开缓存。

`run-latency-existing.mjs --input BASELINE_RUN --input OPTIMIZED_RUN --repeats 1` repeats all queries against completed independent indexes after native test workloads finish. It warms the same fixed model, recomputes every query vector and verifies original chunk/order/score. It writes an independent report without overwriting earlier observations. Model cold load, document inference, generation, evidence selection and web are excluded; unrelated OS/user processes remain untouched.

`run-latency-existing.mjs --input 基线目录 --input 优化目录 --repeats 1` 在其他原生测试结束后，对已完成的独立索引重复全部查询。预热同一固定模型、每次重算查询向量并核验原分块、顺序、分数，另存报告，不覆盖原始观测。排除模型冷加载、文档推理、生成、证据选择和网页；不干预系统与用户其他进程。

Warm latency uses one query at a time after model/index warmup, including fresh query embedding for the end-to-end hybrid entry. It excludes web and answer generation. Five synthetic no-gold identifiers test whether retrieval returns unrelated neighbours; no answers are generated, so this cannot be called a hallucination rate. All fixtures are public or synthetic; results do not prove Chinese/code/memory answer quality.

热态耗时在模型与索引预热后逐条测量，混合完整检索包含重新计算查询向量，排除联网和回答生成。五条无答案标识仅诊断是否仍返回无关邻居，不能称为幻觉率。数据均为公开或合成，不能据此证明中文、代码或长期记忆回答质量。

## 可复用的冻结评测合同

`metrics.mjs` 的 `createRetrievalEvaluationDataset` 固定数据集 ID、版本、development/heldout 标签、语料/查询/金标 SHA-256 和配对身份。金标可以包含多来源与多段真实字面证据；证据必须存在于原文。先保存数据集快照，再运行检索，报告附快照路径与独立校验哈希。标签为 heldout 并不证明数据从未用于调参，必须另外管理留出流程。

`retrievalEvaluationReport` 同时记录 sourceRecall、evidenceRecall、evidenceHit、allEvidence、负例空结果、失败/跳过/诊断，以及包含失败尝试的延迟。失败计入计划分母，缺失或跳过使完整得分保持未知，不能仅报成功子集。无答案空结果只验证检索行为，不测生成拒答或幻觉。开发对照仍是开发回归，不与 BEIR、真实 Agent 或竞品成绩混称。

## Windows 精简旧新配对入口

`run-windows-paired-scifact.mjs` reuses one completed public index and verified real E5 query-vector cache. Default mode only checks identity; `--execute` copies the closed database into two owned roots and alternates old/new production index calls. It never downloads assets or computes missing vectors. It reports original twenty and additional eighty separately; search duration is not relabeled as fresh query-embedding or Agent latency.

本入口复用一次已完成的公开索引及受校验的真实 E5 查询向量。默认只检查身份；显式 `--execute` 才复制到两个独立数据目录，交替调用旧新生产索引。不下载、不重嵌入，缓存缺失明确停止。原二十题与新增八十题分别评分；数据库检索耗时不能冒充真实查询嵌入或 Agent 总耗时。

```powershell
node tests/retrieval-benchmark/run-windows-paired-scifact.mjs --input <completed-public-run> --original20 <query-ids.json> --old-root <old-source-snapshot> --new-root <new-source-snapshot>
```

`query-ids.json` must contain exactly twenty unique official test IDs under `queryIds`, with `origin` recording historical provenance. Missing historical IDs are a blocker; newly frozen IDs must be explicitly labeled as a new sample. The default corpus identity is 5,183 documents / 6,559 chunks. `--expected-chunks 22858` is an explicit different-corpus comparison, requiring the report to retain that distinction; it never silently satisfies the 6,559-chunk request.

二十题清单需要 `queryIds` 数组和如实说明来源的 `origin`。历史题号缺失时不能声称复现原二十题；新冻结题必须另作标记。默认核验 5,183 文档／6,559 分块。显式 `--expected-chunks 22858` 可进行另一语料身份的比较，但不能把该结果冒充 6,559 分块的旧试测。

Add `--execute --stage 20` only after preflight succeeds. For `--stage 100`, `--review` must point to a JSON review with this `pairingId`, `allowExpansion: true`, `resultsPath` and `resultsSha256` identifying a successful complete twenty-query run. The eighty additional IDs are frozen by seeded SHA-256 ordering before either version retrieves; judgments are only used for scoring. Missing or failed queries remain in planned/failed/skipped counts, and successful-prefix metric means are labeled as completed pairs only.

身份检查成功后才能加 `--execute --stage 20`。扩至一百题时，`--review` 指定 JSON：包含本轮 `pairingId`、`allowExpansion: true`、完整成功二十题结果的 `resultsPath` 和 `resultsSha256`。新增八十题在两版检索前按固定种子的 SHA-256 顺序冻结，不按成绩选题。缺失／失败保留在计划、失败、跳过计数中；成功子集均分明确标为完成配对指标。

Stage 100 reuses the completed twenty-query rankings and closed owned index copies; only the eighty additional queries execute. `--execute --latency --completed-run <results.json> --latency-repeats 3` separately times the first three frozen IDs after one unmeasured warm request per query/version. It records actual backend transitions, so a new exact/ANN mixture must not be described as stable ANN-only warm latency. Neither path recomputes query/document vectors.

扩展阶段复用二十题排行与已经关闭的自有索引，只执行新增八十题。另用 `--execute --latency --completed-run <results.json> --latency-repeats 3`，选固定前三题、每版每题先预热一次，再单独计时三次。报告实际后端变化；混合 exact／ANN 观测不能宣称纯 ANN 稳定热态。两条路径均不重算查询或文档向量。
