# 后续 Agent 评测：允许多条正确路径，分开记录语义与成本

核查日期：2026-10-09。本轮读取 BFCL 与 τ 系列作者的官方代码和文档，没有运行它们的模型评测，没有读到其论文全文，也没有修改 KYNXA 生产实现、冻结题目或预期标签。

## 固定来源与实际读取

| 来源 | 固定版本 | 实际读取 |
| --- | --- | --- |
| BFCL 官方 `gorilla-llm/gorilla` | `916260dfc116bf06793a1af79b4ec8195b0453b6` | 根/BFCL README、eval_runner、multi_turn_checker 主要流程与类别定义 |
| τ 系列官方 `sierra-research/tau2-bench` | `4ce7c0397c1eb65c9bbe59aeacfe1ca44a1cd699` | README、完整 evaluation.md、奖励聚合、动作评测主要流程、Action 对比合同 |

第二个仓库保留 tau2-bench 名称，但该提交 README 已称 τ³-bench。本篇讨论这个固定版本，不能把当前新增任务或评测细节直接归给最初的 τ-bench/τ² 论文。源码副本在 `/tmp/kynxa-next-eval-research`，正式复核以固定链接为准。

## BFCL：应调用与不应调用都要测，解码错误单列

[固定评测 runner](https://github.com/gorilla-llm/gorilla/blob/916260dfc116bf06793a1af79b4ec8195b0453b6/berkeley-function-call-leaderboard/bfcl/eval_checker/eval_runner.py#L274)的单轮 relevance/irrelevance 判断，依据能否解出非空函数调用。irrelevance 中，AST 解码失败也被算作没有调用；因此不能把这项分数直接解释为模型正确理解并回答了全部无工具任务。

KYNXA 应分别记录正确直接回答、合法无调用、缺参澄清、输出损坏、工具不可用和错误阻断。模型返回无法解析的工具调用，不能因没有执行而算作正确拒绝；权限闸门挡住一个错误提议，也不能掩盖模型本身误选。

[多轮 runner](https://github.com/gorilla-llm/gorilla/blob/916260dfc116bf06793a1af79b4ec8195b0453b6/berkeley-function-call-leaderboard/bfcl/eval_checker/eval_runner.py#L97)会跳过无法解码的步骤，再调用多轮状态/结果检查；其中额外 `multi_turn_irrelevance_checker` 调用在该版本被注释。仓库定义了某检查函数，不代表默认 runner 使用了它。公开类别中的 miss_func、miss_param、long_context 对本项目有价值，但具体缺参、换题和无工具行为仍要自己建立逐轮断言。

## τ 系列：最终状态与必需事实优先，参考路径不一定唯一

[作者评测文档](https://github.com/sierra-research/tau2-bench/blob/4ce7c0397c1eb65c9bbe59aeacfe1ca44a1cd699/docs/evaluation.md)明确：actions 通常是一条参考轨迹，通过重放生成目标环境；airline/retail/telecom 默认最终奖励由 DB 与 COMMUNICATE 决定，并非要求逐个复现参考工具。ACTION 只有在任务 reward_basis 明列时才影响最终奖励，当前少量 banking_knowledge 任务使用它。

[奖励聚合](https://github.com/sierra-research/tau2-bench/blob/4ce7c0397c1eb65c9bbe59aeacfe1ca44a1cd699/src/tau2/evaluator/evaluator.py#L228)只乘入任务声明的评测分项；diagnostic action match 为 0 不必然表示任务失败。反过来，数据库未变化也不能单独证明回答有依据、拒绝解释正确或完成了资料分析。KYNXA 应同时检查最终效果、所有必需证据和回答支持度。

[Action 参数匹配](https://github.com/sierra-research/tau2-bench/blob/4ce7c0397c1eb65c9bbe59aeacfe1ca44a1cd699/src/tau2/data_model/tasks.py#L178)在 compare_args 未指定时，实际遍历预测调用的参数键。这个诊断函数不能直接拿来校验必需参数或授权：本项目必须由真实工具 schema/broker 检查完整参数与前置条件。这里仅说明评测合同的边界，不据此推断整个官方执行链或分数无效。

## 对当前 KYNXA 诊断的影响

[扩展验证](ambiguity-expanded-validation-20261009.md)中查询规划 36/50 符合、工具闸门 42/58 符合，都属于确定性元数据/闸门探查；不能合成真实模型任务成功率。工程回归 278/278 和 16 个模拟三轮会话也不能证明主模型懂得了这些歧义。

查询剩余 14 条偏差中，多余的授权内只读检索应同时标为效率问题，并核查是否改变答案或任务范围；合法替代取证路径不能仅因与参考步骤不同就判语义失败。用户明确要求不要检索、工具操作越过权限或改变实体/否定/时间时，依然是任务违例。现有标签和数字保持冻结，不能事后改标签提升成绩；下一组留出用独立标准标注。

58 条工具题的 13 处误放行、3 处外语阻断仍保留。误放行标志不是已执行浏览器动作；真实 API 评测须记录能力可见、模型提议、参数校验、实际执行与回执五个阶段。模型碰巧没有用错误开放的能力，不证明闸门正确；闸门阻止错误提议，也不证明模型判断正确。

## 下一组评测设计

1. 冻结当前回归，再由独立表达构建新留出。每题标明用户目标、必要事实、禁止效果、合法动作集合/前置条件和预算，允许多条有效只读路径。覆盖后置否定、转述、条件已成立/未成立、外语肯定、负事实后 retry、显式取消、未知副作用恢复和复合证据。
2. 在隔离模拟执行环境中使用真实主模型决策，固定模型版本、协议、权限模式、工具描述、温度与预算。mock 继续用于协议/状态回归；不能代替语义结论。仅比较一个变化因素，重复运行保留随机波动和全部失败，不选最佳一次。
3. 分开报告：最终任务/必需证据支持、误动作/误阻断、候选覆盖/参数/执行阶段、旧任务污染、错误澄清、重复已完成动作、输出解析失败。自动裁判只评可核对分项；开放答案做盲人工抽查，不依赖单一 LLM judge 自评。
4. 同题记录模型轮次、工具/检索/回读调用数、token、费用、P50/P95 首个有用结果及最终结果。额外无效调用单列成本；合法必要取证不因多一轮而判失败。授权、来源新鲜度、取消与副作用合同始终由实际执行断言验证。

本轮只形成评测方案，没有新的真实模型准确率或时延结果；已有小型词法路径 +0.742 ms 的测量范围不变。
