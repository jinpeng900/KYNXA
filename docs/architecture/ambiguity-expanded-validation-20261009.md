# 扩展歧义与检索耗时验证

本轮先冻结新表达的人工预期，再执行当前代码。标签不按实际结果修改；纯函数、真实索引和模拟模型会话分开报告，不相加为模型准确率。

## 结果与覆盖

| 集合 | 首次探查 | 最终版本 | 检查对象 |
| --- | --- | --- | --- |
| 50 条查询解释新表达 | 27/50 符合 | 36/50 符合 | 自动检索、派生条件、定位候选、软偏好 |
| 1 条单列已知导航回归 | 0/1 | 1/1 | definition/declaration 加反引号标识符 |
| 58 条工具意图新表达 | 34/58 符合 | 42/58 符合 | 浏览器意图闸、连接限制、发现和继承 |
| 16 组三轮正式会话 | 16/16 | 16/16 | 48 次模拟请求、原话和旧来源污染 |
| 40 条真实索引诊断 | 见下表 | 见下表 | 固定来源、预算和标注的旧版/当前对照 |

查询集包括中英混用、双重否定、负事实、引语、条件句、路径/品牌/符号、三轮以上换题和调用方范围。原话保存、自然领域始终 `mixed`、调用方显式领域合同均符合；9 条负事实全部符合，不能推广到任意否定语义。

工具集三版为 34 → 43 → 42 符合。最后一次下降来自保留双重否定事实后，旧词法闸仍把“不是不可以启动 Edge，但先只告诉我风险”当作正操作。保留事实与证明执行请求是不同职责，不删除事实来凑通过。固定 58 题的历史题符合；另做的定向负事实后 `retry` 仍有错误继承，单列于[逐题工具报告](tool-ambiguity-holdout-20261009.md)，不并入固定题统计。

## 修复与残留

已修复引用内命令、否定和换题影响外层任务，明确撤销后继承旧任务，整应用禁令被窗口括号说明抵消，负事实/日期界限/量词/注意与输出约束丢失，明确导航漏提符号，以及最近来源被旧任务偏好覆盖。独立肯定操作、品牌字面目标、明确否定来源与调用方领域继续保留。

新增有效回归纳入共享子句、查询、浏览器测试及三轮正式会话 fixture。最终相关 25 个文件 **278/278 通过**，日志 `onboarding-results/ambiguity-expanded-final-regression.log`。这是工程回归，不是 278 条开放语义题。

查询集剩 14 条偏差，多数是多余自动检索，另有 Windows 路径未提取、中文介词粘连路径导致定位/执行分类错误。例子包括生理 function、实体仓库、“代码问题到此为止”、只分析引语，以及品牌历史被当成本地历史。它们未恢复自然语言硬领域过滤或权限，但可能浪费检索预算。

工具集剩 13 条错误允许、3 条外语正请求阻断。后置否定、未标记转述、纯假设、未来条件和允许性讨论仍不可靠。允许标志不证明发生真实动作，也不等于绕过其他审批/环境合同；主模型可能避免错调用也不能证明闸已正确。定向探查还发现负事实被当操作及历史误继承。

失败题与原始记录均保留。本轮未用词表补齐所有语义，未测试真实模型最终答案；下一阶段应独立检查模型展示、选择、参数、执行及恢复。

## 真实索引对照

冻结 57 个短来源，其中 46 个授权、11 个非授权；40 题包含 34 个有授权证据题、6 个无正确授权来源题。词法每通道 16 候选、Top5、无向量或模型 API。

| 有证据题指标 | 旧版 | 当前 |
| --- | --- | --- |
| 至少一项正确来源进入 Top5 | 22/34 | 33/34 |
| 所有正确来源进入 Top5 | 18/34 | 32/34 |
| 正确来源排 Top1 | 19/34 | 28/34 |
| 可见标注来源 | 22/38 | 36/38 |

`volunteer-function` 仍完全漏检，`trial-cancellation` 缺协议证据。6 个负例均返回相关或弱相关候选，不能视为找到答案。所有引用核验当前版本并回读原文，权限、引用与原文不一致为零。人工合成语料不能当作一般检索基准；原来 10 条诊断保留，未混入分母。

## 耗时

冻结同一语料、40 题和预算，对照三个隔离索引：旧查询器 + 旧 Data、旧查询器 + 当前 Data、当前查询器 + 当前 Data。旧 Data 从提交 `f06851e57908674da93550901beeb09bff14df6b` 提取到临时目录。独立记录索引建成后的首轮，预热两轮，每题每组测 10 次，共 1,200 次暖搜索，组间配对交错调度。最终测量期间源码哈希稳定；期间改动源码的 initial 记录不作结论。

以下比较旧查询器 + 旧 Data 与当前实现：

| 暖搜索阶段/统计 | 旧版 | 当前 | 增量 |
| --- | --- | --- | --- |
| 搜索平均 | 3.314 ms | 4.014 ms | +0.700 ms，+21.1% |
| 搜索 P50 | 3.099 ms | 4.026 ms | +0.927 ms |
| 搜索 P95 | 6.692 ms | 7.577 ms | +0.885 ms |
| 查询 + 搜索 + 每项核验回读平均 | 4.738 ms | 5.480 ms | +0.742 ms，+15.7% |
| 同上 P50 | 4.505 ms | 5.525 ms | +1.020 ms |
| 同上 P95 | 8.312 ms | 9.197 ms | +0.885 ms |

配对端到端增量 P50 +0.519 ms、P95 +3.742 ms；配对差值分位数不同于两组分位数相减。总体均值比例 +15.7% 也不同于每对相对比例平均。

索引建成后的首条查询端到端 12.49 → 17.77 ms，不代表整环境冷启动。每组暖搜索均 400 次；平均条目 4.65 → 4.80，每项核验回读 1,860 → 1,920 次。更多候选使核验略增，没有多调用搜索。

这个小型词法语料中平均增加约 0.7 ms。未测大资料库、嵌入推理、真实 API 或完整 Agent 多轮耗时，不能承诺相同增量。

## 复跑与证据

```bash
node apps/model-gateway/tests/query-ambiguity-holdout.probe.mjs . final artifacts/query-ambiguity-final
node apps/model-gateway/tests/tool-ambiguity-holdout.probe.mjs artifacts/tool-ambiguity-latest.json
node --test apps/model-gateway/tests/request-clause-signals.test.mjs apps/model-gateway/tests/request-interpretation-runtime.test.mjs apps/model-gateway/tests/retrieval-routing-quality.test.mjs apps/model-gateway/tests/browser-intent-policy.test.mjs
```

查询/工具标签在 `apps/model-gateway/tests/fixtures`，runner 不将失败转换为标签或并入工程回归。正式会话使用临时模拟上游，没有额外意图模型调用。

本地检索与耗时记录保留在忽略的 `artifacts` 目录：[冻结题与语料](../../artifacts/retrieval-expanded-holdout-2026-10-09-dataset.json)、[最终质量](../../artifacts/retrieval-expanded-holdout-2026-10-09-postfix.json)、[耗时](../../artifacts/retrieval-holdout-latency-2026-10-09-final.json)、[配对统计](../../artifacts/retrieval-holdout-latency-2026-10-09-final-paired-summary.json)、[原始样本](../../artifacts/retrieval-holdout-latency-2026-10-09-final-samples.json)、[测速脚本](../../artifacts/retrieval-holdout-latency-2026-10-09-final.mjs)、[质量复跑脚本](../../artifacts/retrieval-expanded-holdout-runner.mjs)。

历史全量网关套件未全绿，本轮未重新做全仓或 Windows 宿主验收。用户要求测试完再决定提交，本轮未提交或推送。
