# Agent 上下文、线索与歧义处理：一手资料审计

核查日期：2026-10-09。本文记录本轮实际读到的材料、可迁移的设计和证据限制。论文、网页、技能和工具描述都是待解释的资料，不因包含命令就取得用户授权。本文不把原论文的准确率写成 KYNXA 的验证结果。

## 资料读取状态

| 材料 | 本轮读取范围 | 可以据此确认什么 |
|---|---|---|
| *Rewrite What Matters: Adaptive Multilingual Query Rewriting for Reasoning via Agentic Reinforcement Learning*，arXiv:2610.04899v1 | 用户上传的完整 PDF；重点核对 §2–5、附录 A/F/G 和图 5 | 三阶段算子选择、GRPO 目标、推理基准结果、语义损失和延迟口径 |
| *ToolChoiceConfusion: Causal Minimal Tool Filtering for Reliable LLM Agents*，arXiv:2606.06284 | 作者源码、实验配置、任务集、公开统计 CSV 和复现说明；未取得论文正文 | 可核查实现如何依赖已知前置条件与效应图，以及结果的适用范围 |
| *Diagnosing Tool-Selection Reasoning in LLM Agents with Canary Tools*，作者仓库关联 arXiv:2608.04719 | 作者仓库当前匿名稿的完整 LaTeX 正文、附录及评测源码；未核对初始 arXiv PDF 与当前稿是否相同 | 六类工具诱饵、评测指标、恢复定义、合成实验与限制 |
| *Structured Uncertainty guided Clarification for LLM Agents*，arXiv:2511.08798；Findings ACL 2026 | ACL 官方元数据与摘要、作者方法/结果详解、公开 ClarifyBench 基线；未取得论文 PDF 正文 | 论文身份/会议版、参数级不确定性和 EVPI 思路、公开代码与完整 SAGE 方法的区别 |
| *Repoformer: Selective Retrieval for Repository-Level Code Completion*，arXiv:2403.10059 | 第一作者公开的完整匿名 preprint PDF，重点核对 §3–6 和附录 A–E；作者训练、标注和推理实现 | 选择性检索的训练前提、排名分数的局限、上下文选择与真实检索成本的区别；该 PDF 未核对为 ICML/arXiv 最终版 |
| *Self-RAG: Learning to Retrieve, Generate, and Critique through Self-Reflection*，arXiv:2310.11511 | 官方作者仓库的完整 README、训练说明、短答案及长答案静态推理代码；未取得论文正文 | 按需检索与相关性/支持度/有用性分开评价；专门训练的反思 token 不等于普通 API 提示词 |
| Pi | 官方源码固定提交 `6fb2e7815167e6b19006fc526d1a5d0f5f998787`：提示组装、技能目录、压缩、会话、文件读取与截断；技能文档 | 独立提示段、根据当前工具组装指导、技能元数据与正文分开、原始记录和模型投影分开 |
| Cline | 官方源码固定提交 `fa840c741c3fc2eb49e7e0a4484895a99dae5cc5`：提示组装、文件读取、输出限制、结果缓存与压缩；README | 环境字段、结果分段回读、缓存恢复和工具调用/回执配对 |
| Anthropic SDK / Claude Code | SDK 的 BM25/regex 工具搜索和工具 schema；Claude Code 官方 CHANGELOG | 工具可延后加载，搜索返回工具引用再加载；可提供输入 schema 和例子 |
| Cursor 的动态上下文文章 | 原站 `cursor.com/blog/dynamic-context-discovery` 被代理 CONNECT 403 阻挡，未读正文 | 用户提供的概述可作为待核对的需求，不能写成已审阅的官方算法 |
| Anthropic 三篇工程文章 | `effective-context-engineering-for-ai-agents`、`writing-tools-for-agents`、`advanced-tool-use` 原站均 CONNECT 403，未读正文 | SDK 源码不能证明私有意图判断算法，也不能证明用户提到的自行追加 “2025” 案例细节 |

arXiv 原站与 API、ACL 原站同样受到 CONNECT 403 阻挡；准确名称通过作者公开仓库、Hugging Face 论文元数据和 ACL 官方 GitHub 元数据交叉核对。403 是当前访问结果，不意味着材料不存在，也没有通过更换代理或间接抓取服务绕过目的地策略。资料范围不足时明确标记为源码/作者详解审核，不声称已读到原论文全文。

另外检查了公开原作者资产：ToolChoice 的 `v1.0-arxiv` release 只有源码归档，其 29 项文件没有论文 PDF/正文 LaTeX；ClarifyBench 无 release，作者网站全路径清单只有履历 PDF，没有 SAGE 稿件资产；ACL 官方仓库说明论文 PDF 从官方 webserver 下载，不包含在 Git 源码仓库。未找到可访问的官方全文副本时，保留以上读取限制。

## ToolChoiceConfusion：已知依赖图是重要前提

作者仓库固定提交 `25846d60cbcf1f3b26ee06616a9db6e438a85b19` 的 [README 与引用](https://github.com/R-Suresh/ToolChoiceConfusion/blob/25846d60cbcf1f3b26ee06616a9db6e438a85b19/README.md)确认准确题名和 arXiv:2606.06284。[实验实现](https://github.com/R-Suresh/ToolChoiceConfusion/blob/25846d60cbcf1f3b26ee06616a9db6e438a85b19/code/scaledExperiment.py)与[复现说明](https://github.com/R-Suresh/ToolChoiceConfusion/blob/25846d60cbcf1f3b26ee06616a9db6e438a85b19/REPRODUCIBILITY.md)实际核查结果如下：

- `find_minimal_causal_path` 从已知 `state.keys()` 出发，在人工标注的 `tool.requires` / `tool.produces` 图上 BFS，目标也是预先给定的 `task.goal_state`。
- `filter_cmtf` 只暴露最短路径的第一个工具；`full_causal_path` 暴露整条路径上的工具。两者都依赖准确的目标和状态效应图。
- `run_mock_tool` 接受 `tool_input`，但实际按工具名查任务里的预设输出，**没有使用输入参数决定结果**。这不是完整真实参数正确性的实验。
- 公开主实验是 102 个合成多步骤任务 × 4 个模型 × 6 种展示策略。[公开汇总 CSV](https://github.com/R-Suresh/ToolChoiceConfusion/blob/25846d60cbcf1f3b26ee06616a9db6e438a85b19/results/summary_aggregate.csv)每策略 408 次任务运行：CMTF 和完整路径均 98.53%，全部 100 个工具为 82.84%，词法 top-5 为 60.78%，top-10 为 72.06%。这些是作者公开实验结果，本轮没有重跑。

对 KYNXA 值得采用的是明确的前置条件和可观察状态：编辑前读取目标并取得版本；调用前具备合法参数和授权；回执验证产生的事实。自然语言开放任务通常没有完备 `goal_state` 或可靠的全工具效应图，不能把实验中的 98.53% 当作“每轮只显示一个工具”的依据。发现通道和备选工具应继续可用，模型计划也应能被证据修正。

## Canary Tools：诊断失误，不向用户目录注入假能力

本轮读到的是作者仓库提交 `4c3e8b63867dfad9e3dc1c6a62e120f7bda6dfe6` 的[完整论文 LaTeX](https://github.com/souravch18/mcp-canary-tools/blob/4c3e8b63867dfad9e3dc1c6a62e120f7bda6dfe6/paper/acl/paper.tex)，包含正文、限制和附录；仓库关联 [arXiv:2608.04719](https://arxiv.org/abs/2608.04719)。当前稿使用匿名投稿头，本文不把它误标为已核对相同内容的 arXiv v1。

六类探针分别针对语义相似但功能不对、参数不可满足、夸大能力、忽略前置条件、过时版本和过度特定的作用范围。它们是刻意构造的评测工具，描述及失败语义可控，不能直接充当真实生产工具可信性的分类器；分类标签标识测试中触发的缺陷，不证明模型不可观察的内部推理机制。

实验使用 5 个 MCP 服务器的 12 个真实工具实现，但工具结果是逼真的**合成数据**；120 个单作者模板任务、8 个模型、3 种诱饵密度、3 个工具顺序种子共 8,640 次运行，另有 2,880 次降低明显提示的消融。Task Success Rate 由 LLM judge 评估过程完成，涉及真实世界事实的题目不以事实正确性为评分依据。

必须区分指标：Canary Susceptibility Rate 是每任务 `canary calls / tool calls` 的平均；按类型命中率是出现该类型的任务中至少调用一次诱饵的比例。**第一次调用就计陷阱触发**，即使发现错误并返回正常工具，也仍计触发，但单独计恢复。论文的恢复定义是之后调用正确工具，不等于完整任务已经成功。

作者报告 CSR 与任务成功 Spearman 相关系数 −0.34；模型能力等级不能简单排列所有模型的抵抗性。这说明需要分阶段诊断，不能推出 KYNXA 更换大模型就解决工具误选。论文也承认单作者模板规模有限、供应商与能力等级部分混杂、两种 8B 开放模型代表性不足、LLM judge 有限制；第二 judge 的 κ 为 0.75，人检仅 40 个任务运行。

KYNXA 采用可分开观察的链路：能力有没有进入可见范围 → 模型选择哪项 → 参数和前置条件是否有效 → 工具是否执行/证据覆盖多少 → 是否恢复。正反例和隔离测试可借鉴六类缺陷；生产目录继续只展示真实已配置能力，不插入诱饵工具。

## Structured Uncertainty：缺什么参数，比“模型说没把握”具体

准确题名、作者、页码、DOI 和摘要已核对 ACL 官方提交 `73f3b3aa2f20b6dd71b75a8190c596258243bf5c` 的 [2026.findings.xml](https://github.com/acl-org/acl-anthology/blob/73f3b3aa2f20b6dd71b75a8190c596258243bf5c/data/xml/2026.findings.xml)：`2026.findings-acl.2028`，DOI `10.18653/v1/2026.findings-acl.2028`，2026 年 7 月，40811–40838 页。首次 arXiv 编号为 [2511.08798](https://arxiv.org/abs/2511.08798)，不是另一个 2026 年 6 月的不确定性分解工作。

未取得 PDF 正文。方法和下述具体表格数据来自[作者详细解读源码](https://github.com/MananSuri27/MananSuri27.github.io/blob/0c9c0cea9899288844a85fb959a2976de3e3ca3a/_papers/sage-clarification.md)，与官方摘要交叉核对：

- 候选工具调用允许参数为 `<UNK>`，在工具选择和参数取值域上表示信念；区分用户规格未确定与模型预测不确定。
- 按问题能消除哪些 `(tool, parameter)` 方面的不确定性计算 EVPI，再减去重复询问相同方面的成本；用户回答收缩参数域，净收益低于阈值停止。运行失败可提出修正调用或针对错误的提问。
- [作者公式解读](https://github.com/MananSuri27/MananSuri27.github.io/blob/0c9c0cea9899288844a85fb959a2976de3e3ca3a/_posts/2026-09-07-sage-clarification-explained.md)明确采用 uniform tool prior 和参数条件独立假设：已指定参数记 1，未知有限域记 `1 / |domain|`，未知连续域记 ε。候选和域约束仍由模型解释；域缩到一个值不证明语义理解正确，也不产生执行授权。这种信念不是对开放任务真实正确率的校准。
- ClarifyBench 使用工具日志/BFCL 派生任务和 LLM 用户模拟，含 5 个领域、92 个工具、716 个任务。作者复述表 3 的 GPT-4o 歧义任务中，SAGE Coverage 为 59.73%、平均提问 1.39 次；Domain-aware ReAct 为 55.70%、2.56 次。该 Coverage 指与标准工具调用匹配，不是资料检索覆盖率。
- 并非所有指标都改善：作者复述表 4 的 When2Call ToolCall F1，ReAct 是 0.75，SAGE 是 0.65。减少追问和提升部分完成率仍需权衡错误调用、拒绝和成本。
- 作者解读报告约 22K tokens 的计算成本；不能把复杂候选采样和 EVPI 推断直接称作低成本预处理。公开 [ClarifyBench baseline_agent.py](https://github.com/MananSuri27/ClarifyBench/blob/a85d4f9df1fc87d05c713fd408f8a57d72bcc348/core/baseline_agent.py)是基线，不是完整 SAGE/GRPO 可直接复用的实现。

本轮只迁移参数层面的缺口记录和停止条件：已有路径、工具回执、历史能明确补足的内容先查；真正未确定、会改变动作结果的必要参数才问；记录已解决的参数，避免反复追问。权限确认与信息澄清分开，模型自报 confidence 不当作校准概率，不声称本轮已经实现 POMDP、EVPI 或 RL。

## Rewrite What Matters：值得借鉴的部分与实际限制

### 方法不是“统一翻译提示词”

同一策略模型在三个固定顺序的阶段里分别做规划和改写：语言、结构、语义。语言阶段包括保留、全文英文翻译、关键术语翻译、原文加英文；结构阶段包括条件显式化、按实体组织、简化、重述目标；语义阶段包括指代消解、歧义词解释、计算要求显式化。每阶段都有 `keep_original`。论文并没有按语义问题随意决定阶段顺序。

改写模型和主要下游模型都是 Qwen2.5-7B-Instruct。改写模型接受 GRPO 训练，下游模型冻结；奖励来自最终答案与标准答案是否匹配，**不是检索召回率，也不是用户意图或执行安全的奖励**。附录 G 的提示要求保留事实、数值和约束，但提示要求不等于确定性保证。单独复制这份提示不能算复现训练后的策略。

### 结果与风险必须一起看

表 1 的语言平均准确率如下，均属于论文所用推理任务：

| 基准 | 直接回答 | 全文翻译 | mRewriter-R1 |
|---|---:|---:|---:|
| MGSM 数学 | 65.2% | 68.7% | 73.9% |
| XCOPA 因果常识 | 73.6% | 75.9% | 77.6% |
| BELEBELE 阅读理解 | 75.6% | 75.4% | 80.2% |

- 图 5 的语言阶段 **83.5% 选择全文英文翻译**。所以不能从“选择性”推出“多数输入只做轻量补充”；KYNXA 保留中文原话和原查询分支是本项目的约束，不是论文自动提供的性质。
- 图 6 在 2,750 个 MGSM 样本上修正 402 个原本答错的结果，同时让 110 个原本答对的结果变错。总准确率提高不能保证每条用户请求都改善。
- 附录表 11 的关键数字 token recall 是 93.28%，无数字丢失的查询为 89.07%，币种标记保留为 97.42%，百分比标记保留为 99.71%。语义相似度不能代替实体、数字、否定、时间和范围约束检查。
- 表 3 报平均耗时 0.539 秒；§5 另报非批处理单查询均值 4.655 秒、P95 8.252 秒。统计方式不同，不能用前者承诺新增 API 调用后端到端更快。
- 表 2 的三个维度消融和图 4 的无算子训练失败支持作者在其训练设置中限制动作空间；它们不证明 KYNXA 必须添加另一个 7B 模型或进行 RL 训练。
- 附录表 4 的 MGSM 统计列出 10 种语言、2,500 个测试样本；表 1 与图 6 列出 11 种语言（含 Telugu）、2,750 个样本。该版本存在统计说明不一致，本文沿用各图表自己的明确口径，不把它们合并为已经复核的训练数据清单。

### 本项目采用与不采用的边界

采用有限、可说明的派生：保留原话；只有已确认上下文才能补全指代；准确路径和符号保持原样；需要时补充技术术语或显式条件；新增词说明来源。派生查询只影响候选搜索，不能覆盖用户消息、修改权限、决定操作目标或承诺证据已经充分。

本轮不引入额外改写模型、不训练 GRPO、不默认全文翻译、不让模型猜未明确的删除/编辑目标。跨语言检索是否改善，需要固定模型、来源集和调用预算的独立比较。

## Pi：采用组织方式，不复制整份提示

[系统提示组装](https://github.com/earendil-works/pi/blob/6fb2e7815167e6b19006fc526d1a5d0f5f998787/packages/coding-agent/src/core/system-prompt.ts)把常驻前言、工具、规则、项目上下文、技能和工作目录组织为独立段；工具列表与工具规则来自当前声明的工具，隐藏工具不重复列出。`diffSystemPromptSections` 可以单独更新段，而不是每轮替换全部文本。

[技能源码](https://github.com/earendil-works/pi/blob/6fb2e7815167e6b19006fc526d1a5d0f5f998787/packages/coding-agent/src/core/skills.ts)与[技能文档](https://github.com/earendil-works/pi/blob/6fb2e7815167e6b19006fc526d1a5d0f5f998787/packages/coding-agent/docs/skills.md)先给名称、描述和路径，匹配任务时再读正文；文档也明确模型可能未加载本应适用的技能。目录出现某项能力不证明它已读取或可执行。

[会话源码](https://github.com/earendil-works/pi/blob/6fb2e7815167e6b19006fc526d1a5d0f5f998787/packages/coding-agent/src/core/session-manager.ts)将历史保存为追加式 JSONL 树，`buildSessionContext` 产生当前分支和压缩感知的投影。[压缩源码](https://github.com/earendil-works/pi/blob/6fb2e7815167e6b19006fc526d1a5d0f5f998787/packages/coding-agent/src/core/compaction/compaction.ts)保存目标、约束、进展、关键决定、下一步和具体路径；长度截断或错误的摘要不能成为成功检查点。这支持原始历史与派生任务状态分开。

[文件读取](https://github.com/earendil-works/pi/blob/6fb2e7815167e6b19006fc526d1a5d0f5f998787/packages/coding-agent/src/core/tools/read.ts)和[截断模块](https://github.com/earendil-works/pi/blob/6fb2e7815167e6b19006fc526d1a5d0f5f998787/packages/coding-agent/src/core/tools/truncate.ts)区分行数与字节上限，回执告诉模型下一段的 `offset`；截断是可见状态，不能把头部预览当作全文。

KYNXA 可沿用分段组织和按需加载，但路径权限、ToolHost 与网关环境、来源版本、任务切换、领域过滤都需要本项目合同处理。Pi 源码不是中文歧义分类器，也没有证明通过换提示就能提高代码检索。

## Cline：结果读取与恢复要有程序合同

[提示组装](https://github.com/cline/cline/blob/fa840c741c3fc2eb49e7e0a4484895a99dae5cc5/sdk/packages/shared/src/prompt/cline.ts)注入平台、工作目录、日期、宿主和调用方规则。[Act 提示](https://github.com/cline/cline/blob/fa840c741c3fc2eb49e7e0a4484895a99dae5cc5/sdk/packages/shared/src/prompt/system/act.ts)要求先取得相关上下文、核查工具和实际验证结果。这些原则不能代替实际执行前检查。

[文件读取执行器](https://github.com/cline/cline/blob/fa840c741c3fc2eb49e7e0a4484895a99dae5cc5/sdk/packages/core/src/extensions/tools/executors/file-read.ts)检查文件、取消和模型图片能力；超出窗口时给 `start_line/end_line` 的回读入口。[输出限制](https://github.com/cline/cline/blob/fa840c741c3fc2eb49e7e0a4484895a99dae5cc5/sdk/packages/core/src/extensions/tools/executors/output-limits.ts)把截断提示放在保留的头尾，避免第二次截断恰好隐藏恢复信息。

[结果缓存](https://github.com/cline/cline/blob/fa840c741c3fc2eb49e7e0a4484895a99dae5cc5/sdk/packages/core/src/session/services/tool-result-cache.ts)提供同会话的 `cline://cache/...` 读取入口；缓存有 16 MiB 上限和闲置轮次淘汰，**不等于永久证据仓库**。缓存丢失的错误明确提醒：不要为恢复输出重复有副作用的动作。KYNXA 应复用已有结果归档和来源回读，失效引用要报告失效并重新只读取证，不能把“重新执行修改”当作恢复输出的方法。

[基础压缩](https://github.com/cline/cline/blob/fa840c741c3fc2eb49e7e0a4484895a99dae5cc5/sdk/packages/core/src/extensions/context/basic-compaction.ts)保留工具调用和回执配对，并将删除的工具工作以摘要桥接。是否执行成功仍取决于实际回执，不取决于摘要措辞。

本轮只提炼适用原则，不复制 Cline 的完整模式切换、审批或提示文本。宿主的交互偏好和执行权限必须遵守 KYNXA 当前用户授权。

## Anthropic：公开 schema 可以证明的范围

SDK 固定提交 `50b78d17a8a73bef97c3884102310344ac00f056` 的 [BM25 搜索](https://github.com/anthropics/anthropic-sdk-python/blob/50b78d17a8a73bef97c3884102310344ac00f056/src/anthropic/types/beta/beta_tool_search_tool_bm25_20251119_param.py)、[regex 搜索](https://github.com/anthropics/anthropic-sdk-python/blob/50b78d17a8a73bef97c3884102310344ac00f056/src/anthropic/types/beta/beta_tool_search_tool_regex_20251119_param.py)和[工具 schema](https://github.com/anthropics/anthropic-sdk-python/blob/50b78d17a8a73bef97c3884102310344ac00f056/src/anthropic/types/beta/beta_tool_param.py)说明 `defer_loading`、工具引用、输入 schema 和 `input_examples`。这是“先发现能力再加载合同”的公开接口证据。

[Claude Code CHANGELOG](https://github.com/anthropics/claude-code/blob/e47cc82bdbd27b5f799acd5b05fd0c29d33bb48c/CHANGELOG.md)有 MCP 工具延后加载、描述截断、工具未加载和恢复会话相关修正。本轮核查的版本记录说明这些工程问题确实需要处理，不揭示内部完整意图算法。

搜索命中只是候选能力：BM25/regex 的存在不能推出“关键词就是最终意图”。KYNXA 的程序检查应独立核对已启用工具、环境、参数、权限快照和必要版本；模型结合原话选择，工具结果再修正下一步。

## 补充核查：市面 Agent 的强约束放在哪里

这部分按用户后续要求继续核查公开实现。结论限于下面列出的正式链路，不以若干文件的审阅证明某个项目全仓“绝无关键词规则”。现有 Agent 仍然使用提示规则、词法搜索和格式验证；关键区别是词法线索带来什么后果，以及错误理解能否再取证修正。

### Codex：发现工具与执行工具是两层合同

核查官方提交 `0ada5d8806cdad498230d5b1b2924091e04c8feb`。

- [tool_search_spec.rs](https://github.com/openai/codex/blob/0ada5d8806cdad498230d5b1b2924091e04c8feb/codex-rs/core/src/tools/handlers/tool_search_spec.rs)明示：“Searches over deferred tool metadata with BM25 and exposes matching tools for the next model call.” 输入是模型给出的 `query` 与 `limit`，不是预先指定的 `code/knowledge` 意图标签。来源简介可以常驻或省略，详细工具延后加载。
- [tool_search.rs](https://github.com/openai/codex/blob/0ada5d8806cdad498230d5b1b2924091e04c8feb/codex-rs/core/src/tools/handlers/tool_search.rs)用 `search_scorer.matches(search_embedder.embed(query))` 排序。空 query 和零 limit 返回具体参数错误；Code Mode 候选在 top limit 前核对当前步骤允许的工具与已注册定义。所读实现没有通过 `method`、`network` 等普通词强制改写用户请求或判定检索领域。
- [router.rs](https://github.com/openai/codex/blob/0ada5d8806cdad498230d5b1b2924091e04c8feb/codex-rs/core/src/tools/router.rs)的 `exposes_tool` 检查实际可见、Code Mode 可达或存在搜索入口的 deferred 工具；不是“首轮没展示就不存在”。[registry.rs](https://github.com/openai/codex/blob/0ada5d8806cdad498230d5b1b2924091e04c8feb/codex-rs/core/src/tools/registry.rs)按真实工具名 dispatch，未知工具、payload 类型不兼容、执行前 hook 阻挡分别返回结果或错误。
- [context_manager/history.rs](https://github.com/openai/codex/blob/0ada5d8806cdad498230d5b1b2924091e04c8feb/codex-rs/core/src/context_manager/history.rs)明确工具输出截断只作用于 live history，保留完整 rollout payload 和原始身份/来源元数据；上下文预算不能抹掉可恢复的正式历史。
- [sandboxing/mod.rs](https://github.com/openai/codex/blob/0ada5d8806cdad498230d5b1b2924091e04c8feb/codex-rs/core/src/sandboxing/mod.rs)的执行请求携带文件系统权限 profile、网络代理、沙箱和 Windows 工作区边界。[公开 GPT-5.2 Codex 提示模板](https://github.com/openai/codex/blob/0ada5d8806cdad498230d5b1b2924091e04c8feb/codex-rs/core/gpt-5.2-codex_prompt.md)负责工作方式和结果沟通；该模板不是所有现行模型请求的完整提示快照，也不是通用意图解析算法。

对 KYNXA 的直接建议：能力简介和发现入口保持可达；关键词只影响 discovery 的候选排序；执行约束来自当前真实能力、参数、权限、版本和预算。用户原话依然供主模型理解，后续工具回执可以修正选择。

### OpenHands：确有关键词技能触发，但不是硬领域裁决

当前 [OpenHands 主仓库 README](https://github.com/OpenHands/OpenHands/blob/ff1a1d64f7d7436ea7886a1a0c6c5103faf2a08c/README.md)说明它是 Agent Canvas，正式代理实现归 `software-agent-sdk`。本轮没有把旧 `agenthub/codeact_agent` 路径假称为当前源码，而是核查 SDK 提交 `d1bfda28da7f5c4d3b33d65e729161cef6e0a4ec`。

- [AgentContext](https://github.com/OpenHands/software-agent-sdk/blob/d1bfda28da7f5c4d3b33d65e729161cef6e0a4ec/openhands-sdk/openhands/sdk/context/agent_context.py)区分常驻项目上下文和 `available_skills` 的名称/说明；AgentSkills 默认渐进展示，模型需要时加载。
- [trigger.py](https://github.com/OpenHands/software-agent-sdk/blob/d1bfda28da7f5c4d3b33d65e729161cef6e0a4ec/openhands-sdk/openhands/sdk/skills/trigger.py)公开支持 `KeywordTrigger`、`TaskTrigger` 和真实文件作用域的 `PathTrigger`。`AgentContext.get_user_message_suffix` 的关键词触发附加 `SkillKnowledge`，记录触发来源并避免重复加载；这不是根据单词删掉其他工具，也不是由触发直接执行终端操作。因此不能用“成熟 Agent 完全不用关键词”作为论据。
- [agent.py](https://github.com/OpenHands/software-agent-sdk/blob/d1bfda28da7f5c4d3b33d65e729161cef6e0a4ec/openhands-sdk/openhands/sdk/agent/agent.py)把已配置 `tools_map.values()` 和历史送入 LLM，由模型生成工具调用；之后程序解析 JSON、核对注册名、按 `action_type` 验证参数。未知工具和 ValidationError 形成可见错误事件；执行时的 ValueError 也成为结果事件，让模型根据实际失败修正。
- 同一 Agent 的确认决策独立作用于待执行 action。[confirmation_policy.py](https://github.com/OpenHands/software-agent-sdk/blob/d1bfda28da7f5c4d3b33d65e729161cef6e0a4ec/openhands-sdk/openhands/sdk/security/confirmation_policy.py)提供 Always/Never/风险阈值策略，而非把“发现相关工具”和“获得执行许可”合为一步。风险预测本身也可能由模型产生，不等于确定性证明。
- [静态提示段](https://github.com/OpenHands/software-agent-sdk/blob/d1bfda28da7f5c4d3b33d65e729161cef6e0a4ec/openhands-sdk/openhands/sdk/context/prompts/sections/static.py)指导先探索事实、再分析和验证；[动态提示段](https://github.com/OpenHands/software-agent-sdk/blob/d1bfda28da7f5c4d3b33d65e729161cef6e0a4ec/openhands-sdk/openhands/sdk/context/prompts/sections/dynamic.py)承接环境与技能数据。所读主执行链没有自然语言关键词→检索领域硬排除的步骤。

对 KYNXA 的直接建议：领域词、扩展名和类似符号的大小写名称默认只给偏好；路径存在性、精确符号是否真的出现、sourceRef 是否有效都由工具和引用合同核实。显式调用方传入的检索范围可以限制执行；“模型猜这是代码”不能成为同等强度的范围合同。上游完整提示和默认审批偏好不照搬，用户当前授权始终优先。

### SWE-agent：限制动作接口，保留模型的探索和反馈循环

核查官方提交 `3ea751c087f32b16e039a2233dd6eefecef325d5` 的配置、解析、工具和编辑实现；[SWE-agent 论文](https://arxiv.org/abs/2405.15793)正文未在本轮取得，下面是作者实现审核，不把历史论文描述直接当作当前默认行为。

[config/default.yaml](https://github.com/SWE-agent/SWE-agent/blob/3ea751c087f32b16e039a2233dd6eefecef325d5/config/default.yaml)把问题描述、工作目录和固定 bundle 的工具交给模型；`next_step_template` 将真实 observation 送入下一步。[tools/parsing.py](https://github.com/SWE-agent/SWE-agent/blob/3ea751c087f32b16e039a2233dd6eefecef325d5/sweagent/tools/parsing.py)的 FunctionCallingParser 检查工具是否存在、arguments JSON、必要参数和多余参数，再将调用转为可执行命令。所读链路不根据用户输入里 `method` 或 `network` 判定能力领域。

[tools/tools.py](https://github.com/SWE-agent/SWE-agent/blob/3ea751c087f32b16e039a2233dd6eefecef325d5/sweagent/tools/tools.py)确有 blocklist，但作用对象是模型已经生成的 shell action 前缀或整条命令，用于禁止交互命令等不适合该环境的动作；它不是自然语言意图规则，也不是完整文件系统/网络安全边界。

必须核对实际版本：[ACI 文档](https://github.com/SWE-agent/SWE-agent/blob/3ea751c087f32b16e039a2233dd6eefecef325d5/docs/background/aci.md)介绍过 linter 拒绝语法错误编辑，但当前默认 `edit_anthropic` 的 [str_replace_editor](https://github.com/SWE-agent/SWE-agent/blob/3ea751c087f32b16e039a2233dd6eefecef325d5/tools/edit_anthropic/bin/str_replace_editor)会先写入再附 linter warning；[windowed 编辑器](https://github.com/SWE-agent/SWE-agent/blob/3ea751c087f32b16e039a2233dd6eefecef325d5/tools/windowed_edit_rewrite/bin/edit)才有错误后的 `undo_edit`。因此不能泛称所有 SWE-agent 当前编辑都会被语法闸门阻止。

可迁移的是工具契约：路径存在/绝对路径检查、create 不覆盖已有文件、替换旧文本必须唯一、显示观察和相关行，修改后再验证。这个项目的任务本来就是已知软件工程问题；它不证明 KYNXA 应把某个普通词硬归为代码任务，也不要求照搬固定 Python 仓库提示或禁止改测试的基准规则。

### ToolSandbox：真实状态依赖与结果评测，区别于意图过滤

作者官方项目 [ToolSandbox](https://github.com/apple/ToolSandbox/blob/c8571d7854316d2e1c5f288e59fe1e34e53f6dd1/README.md)对应 [arXiv:2408.04682](https://arxiv.org/abs/2408.04682)。本轮认真核查提交 `c8571d7854316d2e1c5f288e59fe1e34e53f6dd1` 的作者实现与详细 README，未取得论文 PDF；没有重跑其模型实验或借用论文历史分数作为本项目证据。

[execution_context.py](https://github.com/apple/ToolSandbox/blob/c8571d7854316d2e1c5f288e59fe1e34e53f6dd1/tool_sandbox/common/execution_context.py)的工具可用性由 scenario 的 allow/deny 配置决定，[base_role.py](https://github.com/apple/ToolSandbox/blob/c8571d7854316d2e1c5f288e59fe1e34e53f6dd1/tool_sandbox/roles/base_role.py)再按工具角色可见性过滤。[openai_api_agent.py](https://github.com/apple/ToolSandbox/blob/c8571d7854316d2e1c5f288e59fe1e34e53f6dd1/tool_sandbox/roles/openai_api_agent.py)把可见工具 schema 与可见历史送入模型；模型可以向用户澄清，也可以向执行环境发起工具调用。不是先用用户句子的几个关键词决定唯一工具。

一个具体的状态链：[messaging.py](https://github.com/apple/ToolSandbox/blob/c8571d7854316d2e1c5f288e59fe1e34e53f6dd1/tool_sandbox/tools/messaging.py)发送信息前验证手机号，并检查 cellular service；未开启则抛 ConnectionError。[setting.py](https://github.com/apple/ToolSandbox/blob/c8571d7854316d2e1c5f288e59fe1e34e53f6dd1/tool_sandbox/tools/setting.py)开启 cellular 又受 low-battery mode 的实际状态约束。模型依据回执发现和处理隐式依赖，程序约束的是可观测状态，不是“看到发送二字就自动授权”或“看到网络二字就判定诊断任务”。

[evaluation.py](https://github.com/apple/ToolSandbox/blob/c8571d7854316d2e1c5f288e59fe1e34e53f6dd1/tool_sandbox/common/evaluation.py)用 milestone DAG 在执行轨迹和数据库快照上匹配中间/最终条件，并有 minefield 失败条件。比如查联系人与开启服务可并行，但数据库必须出现正确接收人和内容的信息后，才能计已发送成功。这是**有标准目标的离线评测**，不等于开放任务运行时已经知道全部目标，也不是执行权限闸门。

该项目也用词法/fuzzy 相似度：[scenarios/__init__.py](https://github.com/apple/ToolSandbox/blob/c8571d7854316d2e1c5f288e59fe1e34e53f6dd1/tool_sandbox/scenarios/__init__.py)从已经标注的正确工具生成 3/10/all distractor 条件；具体搜索工具也可 fuzzy 匹配数据。它们不能被误解为用词法结果裁决用户意图。

可迁移的是“确认真实状态变化，而不是只看模型说执行了”和“中间步骤、参数、状态与最终结果分开评测”。ToolSandbox 是受控、已知场景与合成设备状态的评测环境，不应把它的固定数据库、gold tool 集和 milestone 图直接用作 KYNXA 的开放式规划器。

### 对本轮实现的修正原则

共同可核查的方向是：让主模型结合原话与当前证据选择下一步；词法检索、领域词和路径外形提供候选或排序；程序守住已配置能力、明确参数/引用合同、授权范围和资源预算；工具错误与覆盖状态回到模型继续判断。删除仅因自然语言推断而形成的硬领域过滤，比再叠一层实体黑白名单更可推广。

这也不要求删除所有规则。范围/权限/版本和必要参数有确定的事实来源，必须保留；从普通单词猜测的“用户只能想要代码/网络诊断”没有同等强度的依据。验证应检查新表达和失败恢复，不能只让 `method/network/react/OpenAI` 四个例子通过后就宣称完成自然语言理解。

## 检索改造：先排序候选，主模型依据证据选择

用户最新要求是把检索候选排序好，交给模型选择。本轮针对这一点增加两项一手审核：Repoformer 能解释为什么不应一律塞入检索上下文；Self-RAG 的作者实现能说明为什么“检索相关”和“支持当前结论”需要分开。下面分别标明真正读到的正文和源码，未把这两套训练框架直接改名为 KYNXA 的提示词方案。

### Repoformer：选择是否使用上下文，不以一个相似度分数裁决

第一作者网站提交 `cf5dfbca96fa6d73e4d9a42012acca14bfedc28d` 的 [完整 preprint PDF](https://github.com/xiaowu0162/xiaowu0162.github.io/blob/cf5dfbca96fa6d73e4d9a42012acca14bfedc28d/files/repoformer_preprint.pdf)已认真读取方法、结果、讨论和附录。作者的[出版记录](https://github.com/xiaowu0162/xiaowu0162.github.io/blob/cf5dfbca96fa6d73e4d9a42012acca14bfedc28d/_publications/8_repoformer.md)将该工作关联到 ICML 与 arXiv:2403.10059，但 PDF 首页仍为 Anonymous Authors，正文新基准叫 CCEval，而当前作者摘要叫 CrossCodeLongEval；没有把两个版本的名称与分数混在一起。下载 PDF 的 SHA-256 为 `a1c0d0ed5bb0817a2d2f5ea730d13a7d784ea0809a9da9fccadb88a43b077328`。

正文 §3.2–3.3 的方法是训练模型判断：给定当前文件左右文，额外跨文件资料是否会改善补全。训练时对相同样本分别生成“无检索”和“有检索”补全，与真实目标比较 Edit Similarity，再以差值产生标签；`<eof>` 后的 `<cc>` 特殊 token 概率控制是否检索，同时训练生成能力。附录 D 部分训练查询会加入已知目标代码，以制造有用/无用检索的监督差异；这不是生产推理可以访问未生成的正确答案。主实验从 18,000 个 Python 仓库抽取 240,000 段和 120,000 个函数样本，微调 StarCoderBase-1B/3B；不是更换系统提示就获得的能力。

附录 C 很贴合本次改造：只看 top-1 检索相似度，可以减少送入生成的跨文件资料，但**已经进行过检索，所以不节省这一次检索延迟**；它也没有考虑模型当前已有的知识。20 次采样估计熵或最低 token 概率，在长函数任务上的效果不稳定。由此可借的是“相关资料未必需要采用”，不能把 embedding/BM25 分数叫作正确答案概率，或用一个固定阈值代替主模型对任务的判断。

结果与边界要一起看：匿名稿表 3 的 1B API 补全，始终检索 ES 为 72.02；按 `<cc>` 最大概率选择为 71.04，速度提升 69%；按概率阈值为 72.72，速度提升 28%。附录 E.3 文字写概率阈值可带来超过 70% 提升，但紧邻表 8 的概率阈值行只有 21%–33%，超过 70% 的是另一种 self-selection；因此不引用摘要或这一句作为“无损 70% 加速”的依据。§5.3 在线延迟模型还并行启动决策、无检索生成、检索加生成，最后等待被选中的分支并忽略另一分支；这主要是关键路径延迟模型，不能自动等同为总计算、全部检索或 API 费用降低。

附录 E.1 承认函数补全尤其以单测结果衡量时，检索概率校准不足；使用文本相似度标注，不能保证行为正确。讨论还把多次动态检索列为未来工作，因此不把 Repoformer 描述成已经验证了开放任务的完整补查循环。

作者仓库提交 `5b0571318e9918fd2af132c4b35077f9ea331133` 的 [训练预处理](https://github.com/amazon-science/Repoformer/blob/5b0571318e9918fd2af132c4b35077f9ea331133/finetuning/preprocess/preprocess_repoformer.py)与 [vLLM 推理样例](https://github.com/amazon-science/Repoformer/blob/5b0571318e9918fd2af132c4b35077f9ea331133/repo_eval/eval_vllm_repoformer.py)也已核查。样例从数据读取预先准备的 `crossfile_context`，并硬编码 token 索引；当前 `do_retrieval=True` 分支却生成不含 CFC 的 prompt，与正标签训练含 CFC 的方向不一致。这是本轮静态核查到的样例问题，没有运行确认作者发布 checkpoint 的真实表现，也不直接复制该分支。

对 KYNXA 的迁移：主模型可以从当前证据判断是否需要检索、候选是否值得回读；检索排序负责把可能有用的资料排到前面。论文训练出的 selector 可以作为另一个模型驱动黑箱代码补全，但普通 API 主模型没有该特殊 token 与校准前提，本次不增加一个 Repoformer 模型或承诺复现其收益。

### Self-RAG：相关性、支持度和回答质量分开观察

本轮核查官方提交 `1fcdc420e48f50a7d7ab1ece5494221b93252e99` 的 [完整 README 与训练说明](https://github.com/AkariAsai/self-rag/blob/1fcdc420e48f50a7d7ab1ece5494221b93252e99/README.md)、[短答案推理](https://github.com/AkariAsai/self-rag/blob/1fcdc420e48f50a7d7ab1ece5494221b93252e99/retrieval_lm/run_short_form.py)和[长答案静态推理](https://github.com/AkariAsai/self-rag/blob/1fcdc420e48f50a7d7ab1ece5494221b93252e99/retrieval_lm/run_long_form_static.py)。arXiv/OpenReview 原站不可访问；官方项目站及作者公开网站仓库中也未取得正文资产，下面明确是作者实现审核，不报论文表格收益或冒称全文阅读。

训练说明明确有 Critic 与 Generator：先使用 GPT-4 创建反思标注并训练 Critic，随后标注生成训练数据；生成模型以 next-token objective 学习 `[Retrieval]`、`[No Retrieval]`、`[Relevant]`、`[Fully supported]`、`[Utility:*]` 等特殊 token。默认模型是专门训练的 Llama2-7B/13B，检索器采用 Contriever；不是每次调用普通 API 时附几条要求就等价实现。

短答案实现将候选段落分别拼入 `<paragraph>` 后生成，再按相关性、完整/部分支持度、有用性及可选序列分数打分；长答案实现对生成片段进行 beam 搜索，用 `beam_width` 与 `max_depth` 控制搜索量。**高检索相关性不自动等于某句话得到资料支持**，这是值得迁移的区别。但这些分数来自专门训练的 token 分布，仍是模型判断，不是事实正确性的确定性证明，也不是执行授权。

作者代码同时暴露适用边界：静态长答案评测将已带来的 `ctxs/docs` 截取 `ndocs`，后续分支仍使用这一组候选；README 另外提供真正在线的 retriever demo。因此“可以继续使用或再次判断证据”不证明静态评测实现了每个信息缺口的新查询。短答案阈值分支还直接使用 logprob 比值，而长答案首轮使用指数转换后的概率；本次不照抄这些样例为普通 API 计算所谓置信度。

对 KYNXA 的迁移：把排序结果作为可选择的资料，保留来源入口；主模型回读原文后判断其能支持什么、还有哪个信息缺口。API 主模型可以用明确工具调用表达选择和补查，无须安装 Critic、Contriever 或 Self-RAG 生成模型。若以提示词让 API 返回支持度判断，只能称该设计借鉴了检查维度，不能称已复现 Self-RAG 的训练方法或论文指标。

### 适合当前 API 架构的具体流程

1. 保留用户原话和当前任务状态。主模型已有足够来源时直接使用；发现明确缺口时才生成派生查询，记录指代补充或多语言扩展的依据。是否检索由语境和证据决定，不由几个自然语言词触发硬开关。
2. 在已授权的来源范围内检索，将精确路径/符号匹配、词法、语义及领域偏好用于召回和排序。排序分数表示候选优先级；只有调用方明确传入的范围约束才硬过滤，弱领域线索不删除其他允许来源。
3. 向主模型提供候选的 rank、标题/路径、简短命中片段、来源类型、可回读的 `sourceRef`、版本信息及检索覆盖状态。列表与片段受上下文预算约束，仍保留翻页/回读入口；排名第一不自动采纳，单个来源也不因重复切片填满可见列表。
4. 主模型决定回读哪些候选及是否需要更多证据；程序只验证引用仍有效、版本匹配、读取范围及参数合法。回读后区分“与任务相关”和“能支持当前结论”，引用真实原文，而不是用检索片段或排序分数证明结论。
5. 只有具体缺口、失效引用或覆盖不足才补查；查询、来源和已读版本去重，依据真实回执更新状态。调用/时间/token 预算与实际证据进展控制循环；不把主模型自己给出的置信度阈值当作正确性或授权闸门。
6. 无新证据且继续检索没有进展、达到预算、工具不可用或关键条件缺失时，给出具体停止/恢复状态。当前允许领域已经按偏好共同召回时，不为修正本可避免的硬过滤再平白追加一次扩大搜索。

这段是依据一手材料和 KYNXA 现有接口提出的工程方案，并非某篇论文已经证明的完整组合算法。应分别测候选覆盖、排序、模型选读、证据支持、补查收益和调用成本，才能判断“排序供模型选”在本项目的实际收益。

## KYNXA 的可检验设计原则

| 设计约束 | 原因 | 可观察验证 |
|---|---|---|
| 原始请求和派生查询分开 | 原文提供实体、否定、时间与范围的最后依据 | 日志保留 `originalQuery`；每个补充项附依据，修改原文的候选不可自动接纳 |
| 普通词和大小写名称只提供偏好 | `method`、`network`、`react`、`OpenAI` 都可能属于多个领域 | 词法排序可以提高候选，弱线索不能硬排除文档/代码或直接选择终端 |
| 有效路径、完整符号和已回读引用可提供明确对象 | 真实定位依据与自然语言形状不同 | 显式路径仍准确匹配；引用须通过来源/版本合同，未核验名称仍为候选 |
| 任务关系显式记录并可修正 | 继续、纠正和换话题需要不同的上下文继承 | 续问只继承已确认对象；纠正撤销被改条件；换话题不混入旧关键词，原始历史保留 |
| 主模型理解，程序验证执行条件 | 模型有语境能力，但输入/权限/版本有可确定的合同 | 工具未加载、参数非法、缺 hash、权限不足、执行环境不匹配分别给具体状态 |
| 信息缺口先只读取证，再决定是否提问 | 模型不确定不等于用户没有给信息 | 有已授权路径或引用时先读；只有影响结果且不能安全推断的关键参数才澄清 |
| 弱领域线索直接搜索允许的领域并集，权限范围固定 | 错领域和覆盖不足都不能证明资料不存在 | 领域偏好参与排序；授权 scope 不增加；不为纠正可避免的硬过滤平白增加第二次调用 |
| 失败状态决定恢复动作 | 重复原调用可能浪费预算或重放修改 | 未命中、覆盖不足、来源改变、工具不可用、参数非法、执行失败、结果失效分别恢复 |

基础提示写共同原则，工具描述写特定用途、前置条件和少量正反例，技能写流程。不要把同一约束在三处逐字重复维护。`method` 与论文方法、`network` 与人际网络、`react` 与普通反应、品牌与符号应作为独立对照；新表达留作未参与调参的验证集。

先在固定来源、模型和预算下分阶段比较：能力是否展示、工具是否选对、参数是否有效、检索是否误过滤、改写是否丢约束、失败是否可恢复、总调用/耗时是否增加。通过确定性回归只能证明合同边界；没有真实模型调用就不能声称模型意图准确率提高。

## 可复核标识与待补资料

上传 PDF 的 SHA-256：`ab6bf5146dc4bd0025d478cc35f894f01d937cf26a7304d3b4a368b3d0a6c729`。来源：[arXiv:2610.04899v1](https://arxiv.org/abs/2610.04899v1)；正文按用户上传版本读取，未声称在线下载成功。官方项目源码本轮读取成功，固定提交用于避免后续主分支变化影响审计。

Cursor 和 Anthropic 原站工程文章尚待可访问的原文核对；没有补全厂商私有算法或把用户概述当作已读正文。已读源码可以独立支持上述接口和上下文组织原则。
