# 后续检索优化：按需触发、具体缺口与调用成本

核查日期：2026-10-09。本轮只研究方案，没有改生产实现或重新运行模型实验。以前已读的 Repoformer、Self-RAG 与 Agent 源码见 [一手资料审计](agent-context-source-review-20261009.md)；本篇新增 Adaptive-RAG、IRCoT、FLARE 三项作者资料，针对当前残差提出可验证的下一步。

## 当前问题与优先级

[扩展验证](ambiguity-expanded-validation-20261009.md)保留了 14 条查询解释偏差和 16 条工具边界偏差。查询偏差多数是多余自动检索，另有 Windows 路径/中文介词粘连路径问题；工具偏差为 13 条语义误放行、3 条外语明确请求阻断，定向探查还发现负事实后 `retry` 继承旧操作。

这两类问题不能合成一个“RAG 准确率”。检索论文可以帮助决定何时查、查哪个缺口、何时停止；它们没有证明能正确识别后置否定、转述或执行授权。工具边界应另做模型动作与当前任务状态的接口设计，不能以“检索更准”宣称修复误授权。

建议先把“是否检索”和“还缺什么证据”交给现有主模型，在同一工具循环表达决定；程序验证来源范围、引用版本、参数和预算。减少自然语言关键词触发的无效自动调用，比先增加一个隐藏分类模型或每句话预生成后再重写更符合当前 API 架构。

## 本轮真正读到的材料

| 工作 | 读取范围与冻结来源 | 不能声称的内容 |
| --- | --- | --- |
| Adaptive-RAG，arXiv:2403.14403，NAACL 2024 | 作者官方 `starsuzi/Adaptive-RAG`，提交 `0c88670af8707667eb5c1163151bb5ce61b14acb`：README、成功标注、T5 分类训练/预测、分类后答案选择和步骤计数 | 未取得论文正文，不把源码步骤代理写成论文墙钟耗时或美元收益 |
| IRCoT，arXiv:2212.10509，ACL 2023 | 作者官方 `StonyBrookNLP/ircot`，提交 `3c1820f698eea5eeddb4fba3c56b64c961e063e4`：README、检索/生成/退出控制器、Codex 配置与 API 生成器 | 未取得论文正文，不借用论文摘要中的提升数字，不把 FLARE 重实现的 previous-sentence 基线等同于原始 IRCoT 实验 |
| FLARE，arXiv:2305.06983，EMNLP 2023 | 第一作者 Zhengbao Jiang 公开博士论文 *Towards More Factual Large Language Models*，2024-05-22，第 7 章，正文页 91–108；官方代码 `jzbjyb/FLARE`，提交 `ec4b06b502b5ab54f3f9236b0112a5d28482e7bb` | 该章明示对应原工作，但没有逐页核对 EMNLP/arXiv 最终 PDF，不冒称全本博士论文已读或当前所有 API 都返回所需概率 |

arXiv/ACL/OpenReview 原站受到当前代理目的地策略限制，未绕过。Adaptive-RAG 的作者网站和仓库未取得公开正文资产；CRAG 作者仓库作为备用核查，同样没有取得正文，本轮未把它追加为第四个已读论文。缺乏可访问资产不意味着论文不存在。

## Adaptive-RAG：选择成本合适的策略，需要实际监督

[官方 README](https://github.com/starsuzi/Adaptive-RAG/blob/0c88670af8707667eb5c1163151bb5ce61b14acb/README.md)描述三个分支：不检索、单次检索、多步检索。关键实现不是按几个词推断复杂度，而是另外训练分类模型。

[evaluate.py](https://github.com/starsuzi/Adaptive-RAG/blob/0c88670af8707667eb5c1163151bb5ce61b14acb/evaluate.py)以 normalized exact-match 标记三种策略各自答对哪些问题；[preprocess_utils.py](https://github.com/starsuzi/Adaptive-RAG/blob/0c88670af8707667eb5c1163151bb5ce61b14acb/classifier/preprocess/preprocess_utils.py)的 `label_complexity` 依次赋 C、B、A，后者覆盖前者，选择成功策略中检索步数最少的一项；三项全失败的样本跳过。代码还把 NQ/TriviaQA/SQuAD 统一标 B，把 MuSiQue/HotpotQA/2Wiki 统一标 C，作为数据集偏置标签。

这使“复杂度”依赖选定模型、检索器、语料和答案指标，而不是题目具有一个永恒的简单/复杂属性。全失败样本也无法给出“再检索就能回答”的监督。更换 API 模型、来源语言或任务类型后，不应默认原分类器仍正确。

[训练脚本](https://github.com/starsuzi/Adaptive-RAG/blob/0c88670af8707667eb5c1163151bb5ce61b14acb/classifier/run/run_large_train_gpt.sh)明确使用 `t5-large`，学习率 3e-5、batch 32、35/40 epochs、max sequence 384；[分类实现](https://github.com/starsuzi/Adaptive-RAG/blob/0c88670af8707667eb5c1163151bb5ce61b14acb/classifier/run_classifier.py)取生成首位置 A/B/C logits 进行 softmax 与 argmax。普通 API 主模型口头说“复杂”不等于这个训练后的策略，也不能用其自评当可靠概率。

[postprocess_utils.py](https://github.com/starsuzi/Adaptive-RAG/blob/0c88670af8707667eb5c1163151bb5ce61b14acb/classifier/postprocess/postprocess_utils.py)从已经离线运行好的三种系统答案文件中选取输出，并计 A=0、B=1、C=记录的多步数。这个步骤代理不包含全部生成调用、分类器推理、token 或实时延迟。作者 [silver validation 预处理](https://github.com/starsuzi/Adaptive-RAG/blob/0c88670af8707667eb5c1163151bb5ce61b14acb/classifier/preprocess/preprocess_silver_valid.py)还使用 test 执行结果形成分类验证标签，README 明示这一过程；本项目应独立冻结开发、验证和留出数据，不直接复制该选择流程。

可迁移的是三种策略都合法：资料已足够时不查；一个缺口单查；确有依赖的多跳问题再逐步查。零检索以已有有效证据或任务不需外部事实为前提；需要确认当前仓库事实、实时状态或执行前提时，不能用分类为 A 或模型声称知道答案代替取证。当前不引入额外 T5 分类器；由主模型在既有回合选择工具，通过真实结果修正。只有将来取得足够项目数据、完整失败样本和跨模型验证，再考虑训练独立策略。

## IRCoT：随新证据补查，循环必须有界

[作者 README](https://github.com/StonyBrookNLP/ircot/blob/3c1820f698eea5eeddb4fba3c56b64c961e063e4/README.md)明确原实验支持 Codex API 和 FLAN-T5；不是必须安装检索决策模型。旧 Codex 已停用，代码主要使用 completion API；迁移当前 chat/tool API 需要调整接口，不能直接运行旧配置就称适配完成。

[ircot.py](https://github.com/StonyBrookNLP/ircot/blob/3c1820f698eea5eeddb4fba3c56b64c961e063e4/commaqa/inference/ircot.py)的 `RetrieveAndResetParagraphsParticipant` 首先使用原问题，随后可以使用最后生成句作为 BM25 查询；生成器读取当前累计段落再生成下一句。它会核对返回 corpus、跳过过长段落、按标题和段落 fuzzy 相似度去重，保留已取证内容。退出控制器在答案格式匹配、生成空句或达到句数上限时结束。

[MuSiQue API 配置](https://github.com/StonyBrookNLP/ircot/blob/3c1820f698eea5eeddb4fba3c56b64c961e063e4/base_configs/ircot_qa_codex_musique.jsonnet)每次 BM25 取 6 段，全局最多 15 段；生成/退出控制器默认最多 10 句。代码的段落上限不等于每次都必然提前停止，真正循环仍由句数/输出退出条件控制。不能只限定候选数量却放任重复模型回合。

可以借用“先前回执暴露下一缺口”，但不把每个生成句直接当事实加入查询：模型可能生成错误实体，自动用它补查会强化错误。KYNXA 应传递明确待证实的子问题和已回读来源；未证实实体保留为候选。无须存取或展示模型完整内部思考，用可核查的缺口、查询、引用和结果就能实现这个工程接口。

[gpt3generator.py](https://github.com/StonyBrookNLP/ircot/blob/3c1820f698eea5eeddb4fba3c56b64c961e063e4/commaqa/models/gpt3generator.py)有确定温度下的磁盘缓存、上下文 token 限制和 API 重试；README 明示多系统/多参数实验费用可能累积很快。原代码的长等待重试和基准专用答案正则不适合作为开放 Agent 的默认恢复策略。对本项目只采用明确调用预算、结果去重、错误分型与可恢复来源入口。

## FLARE：主动查下一缺口，比机械重复上句话有意义

已精读第一作者公开博士论文的 [第 7 章原文件](https://github.com/jzbjyb/jzbjyb.github.io/blob/e858afc35e7cb1c30d557e783469f9867aecd539/paper/thesis_zhengbaojiang.pdf)，方法、实验、消融和效率讨论均核对。PDF SHA-256 为 `b774fefd36e92691653c21c96cb8e3d500e6de968a36560bb749fe7228f7776b`。作者 §7.3 区分两种方法：

- FLARE-instruct：用指令与 few-shot 例子让模型在需要时生成搜索调用。
- FLARE-direct：先生成临时下一句，若含低概率 token，则用屏蔽低概率词后的句子或为不确定片段生成的问题检索，随后重新生成句子。

作者 §7.3.2 明确指出：直接用错误的候选答案做查询可能让检索强化错误。因此“Biden 在 Pennsylvania 上大学”应查“Biden 在哪所大学”，而不是把尚未核实的 Pennsylvania 当成检索条件。这与保留原话、补查中性问题的原则相符。

§7.6 的结果也不支持“一律多查”：表 7.4 中 StrategyQA 单次检索 EM 为 68.6，低于无检索 72.9；FLARE 为 77.3。作者分析超过一定检索比例后会加入噪声；阈值还按任务在 dev 集选取，不能把 0.8 变成 KYNXA 的通用正确性标准。表 7.3 的 2WikiMultihopQA，单次检索/FLARE-instruct/FLARE-direct EM 分别为 39.4/42.4/51.0；作者明确这些 previous-window、previous-sentence、question-decomposition 基线是为了公平比较重实现，并不是对应论文的精确复现。

§7.6.2 的效率讨论承认：比起单次检索，交替生成与检索需要多次激活模型；没有缓存还会反复计算此前前缀。这篇没有为 KYNXA 提供实时 API 延迟或费用保证。只减少检索次数不代表省掉预生成、问题生成和重新生成的模型成本。

官方 [openai_api.py](https://github.com/jzbjyb/FLARE/blob/ec4b06b502b5ab54f3f9236b0112a5d28482e7bb/src/openai_api.py)与 [templates.py](https://github.com/jzbjyb/FLARE/blob/ec4b06b502b5ab54f3f9236b0112a5d28482e7bb/src/templates.py)核实了这一依赖：completion 分支从 token logprobs 转为概率；当启用过滤/屏蔽，`ApiReturn.use_as_query` 要求 token 数据。原 chat 分支只构造文本结果，没有这些 token 数据。默认 `max_iteration=10000` 也不是可直接采用的开放任务预算。当前 API 若不给所需概率，不能用口头置信度或另一模型评分冒充原算法。

本项目值得借的是“查待回答的下一缺口”和“去掉尚未验证的答案断言”，不是默认逐句预生成与再生成。明确证据需求由主模型在现有工具回合表达；不增加隐蔽优化模型调用。候选句只作为待证实资料，也不会因此取得执行权限。

## 针对实际残差的可行改造顺序

| 已观察问题 | 下一步可审查方案 | 来源支持与边界 |
| --- | --- | --- |
| 生理 function、实体仓库、只分析引语、话题结束仍自动查 | 自动规则只保留检索提示；主模型结合原话与当前任务在既有回合决定是否发起真实检索。明确附件/引用可以提供候选入口，不能把所有模糊表达预检索 | Adaptive-RAG 支持零/单/多步策略共存；未证明主模型一定正确，需真实调用测试 |
| `volunteer-function` 中英语义不匹配，完全漏检 | 主模型保留原查询，追加有依据的英语概念候选，如 volunteer group、social function、limited contacts；先召回再回读，记录新增词，不追加年份或硬领域 | 借 FLARE 的中性信息需求思想与此前受约束多语言改写；不是新增全库硬词表 |
| `trial-cancellation` 找到 abort 代码却缺 withdrawal 协议 | 将“程序是否取消”和“退出同意是否合规”作为两个证据需求；排序列表允许不同来源，回读后仅对缺失协议补查 | IRCoT 的按证据推进；两个分支独立时可并行，依赖新事实的补查保持顺序 |
| Windows 路径漏取、中文介词粘连路径 | 路径外形先作为定位候选；使用 Windows-aware 规范化和真实路径工具确认。无法确认时给明确定位状态 | 属于确定性定位接口，不把检索论文写成路径解析算法，也不凭扩展名锁定领域 |
| 无答案的题仍返回弱相关候选 | 显示覆盖/来源状态，主模型回读后检查资料是否支持当前结论；“有候选”“有相关证据”“足够回答”分开记录 | 排名不是答案概率；继续查需具体缺口，不能用重试掩盖无证据 |
| 工具误放行、外语阻断、负事实后 retry 旧操作复活 | 与工具接口设计分开治理：发现候选、模型提出当前动作、真实前提与当前任务状态验证、执行回执分别记录 | 三篇检索工作均不证明授权语义；不继续扩大关键词许可/禁令词表来承诺彻底解决 |

补查预算同时计模型回合、搜索、回读、token、墙钟时间与新增有效来源数。重复查询、相同版本来源和无新证据回合去重；来源失效则明确刷新版本，工具不可用则更换可用取证路径。预算用来控制成本，不拿弱语义置信度限制所有候选。

排序和展示可先做便宜改造：相同来源切片归并、来源多样性、短片段与回读入口、按任务状态更新候选；相互独立的来源读取可并行。缓存必须绑定查询/作用域/权限状态和来源版本，不能把缓存存在当成当前仍获授权。避免将“更少工具可见”“更少回读”单独当优化成功，相关证据覆盖和任务完成也必须保留。

## 怎样验证是否真的减少误判与时延

目前 40 题小词法语料中，当前查询、搜索和逐项核验回读均值 5.480 ms，相对旧版增加 0.742 ms；这没有包含语义嵌入、外部资料、大索引或真实模型调用。不能把 API 主模型的多轮网络等待折算为这一毫秒增量，也不能预设“检索更少一定更快”。

先固定模型、语料、授权范围和总预算，以相同题目比较：现有自动检索、主模型按需检索、按需加具体缺口补查。每次只变一个机制；保留当前 14/16 条作为开发回归，另冻结未参与本轮设计的新表达与多语言留出题。真实 API 验证同时记录：

- 是否需要检索、候选覆盖、被选读来源、结论支持度和最终任务完成。
- 误调用与错误阻断、引用版本与权限违例、换题后旧任务污染。
- 主模型调用数与输入/输出 token；搜索/嵌入/回读各阶段耗时；首个有用结果与最终回答 P50/P95。
- 缓存命中、新来源收益、补查无进展、预算耗尽及取消后的多余计算。

只有结果与成本一起改善，才能认为采用按需策略有用；错误地跳过用户要求的搜索或工具执行，不能算节约。没有实测 API 最终选择与结果前，本篇只给方案，不宣称已经降低当前 14/16 条误判或完整 Agent 时延。
