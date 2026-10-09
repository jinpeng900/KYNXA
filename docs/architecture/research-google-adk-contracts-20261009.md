# Google ADK：工具选择、前置条件与执行合同核查（2026-10-09）

Google ADK 可以借鉴的是工具调用前后的扩展点、结构化输出、事件与确认恢复合同。它没有公开实现一个能够证明任意自然语言条件、否定范围或用户授权的通用意图判断器。用 ADK 替换 KYNXA 的提示词，不能据此宣称解决当前 58 题中的语义残差。

本次只读作者仓库源码、官方文档源码和对应上游测试，没有安装 ADK、调用模型、运行浏览器或修改 KYNXA 生产代码与测试。没有读取厂商私有实现，也没有测得模型准确率或毫秒级时延。

## 固定来源

| 来源 | 固定提交 | 提交日期 | 获取方式 |
| --- | --- | --- | --- |
| `google/adk-python` | `128faabb6dddacf26e029aa8c28c903c3a7a6b61` | 2026-10-08 | `git ls-remote … HEAD` 后浅克隆并核对 HEAD |
| `google/adk-docs` | `57e34aae019b97da72b6d739f5842781bca3278c` | 2026-10-08 | 同上 |

抓取副本在 `/tmp/kynxa-research-google-adk` 和 `/tmp/kynxa-research-google-adk-docs`，不纳入仓库。以下全部上游源码链接固定到上述提交。HEAD 不等于已发布稳定版本；此次文档中还包含标为 v2.0.0 的图工作流接口和实验性确认能力。

## 实际执行链

| 层次 | 源码真实行为 | 能证明什么、不能证明什么 |
| --- | --- | --- |
| 工具集合 | `get_tools(readonly_context)` 返回工具；`tool_filter` 接受名字列表或应用提供的 predicate。[S1][S2] | 决定给模型展示什么。框架没有在这里验证用户句子的真实含义。 |
| 模型请求 | 工具解析结果注册到 `llm_request.tools_dict` 和函数声明，主模型返回函数调用。[S3][S4] | 模型提出动作及参数。候选可见和模型选中都不是执行授权。 |
| 调用前检查 | plugin 回调先运行，再运行 agent 的 `before_tool_callback`；非 `None` 返回值替代结果，真实工具不运行。[S5] | 应用可检查当前权限、目标、前置状态和参数，也可返回缓存结果。回调本身不自动获得语义理解。 |
| 确认与执行 | 无回调替代时，统一 confirmation gate 再决定暂停、拒绝或执行。[S6] | 应用配置的确认策略及对应回执。不是模型的 confidence/reason。 |
| 调用后处理 | `after_tool_callback` 可替换给模型的结果；工具上下文的 state 改动进入事件 delta。[S5][S7] | 可以归一化观察结果、保存真实回执。工具已发生的副作用不能靠替换结果撤销。 |

工具集合解析使用 `asyncio.gather` 并发重叠 MCP 等列举 I/O，随后按原工具顺序串行修改模型请求；这是降低列举等待的实现，不是另一次模型选择。[S3]

不能把“动态工具集合”描述成每个工具都一定实时重算：`BaseToolset.get_tools_with_prefix` 默认按 invocation ID 缓存解析结果。[S1a] MCP 的 `get_tools` 内部确实在每次进入该方法时重新应用 predicate，但外层 invocation 缓存可能直接返回；SkillToolset 则显式关闭该缓存。[S2][S8] 因此真正影响权限和前置条件的检查仍应在执行前进行，不能只靠曾经过滤过的候选。

### 回调与参数检查的具体限制

当前源码的 callback chain 使用 `result is not None`，所以空字典 `{}` 也会停止链并跳过执行。[S5][S9] 官方文档的一段说明仍称空字典不会停止回调链，与此次固定源码不一致；这里以源码为准，不能直接照抄该段文字。[D1]

`FunctionTool` 会检查必要参数并返回错误，让模型补齐参数；完整类型验证受实验 feature flag 控制，默认关闭，某些不支持的 annotation 还会跳过验证。[S10][S10a][S11] 上游测试明确覆盖“默认不拒绝将整数传给字符串参数”。[T1] 因此 ADK 的 schema 展示不等于默认强制完整类型、业务目标或权限验证。KYNXA 已有的确定性参数、路径和版本检查应保留。

## 结构化输出与计划

`LlmAgent.output_schema` 约束最终响应形状。支持同时使用 schema 和 tools 的模型直接配置输出 schema；其他模型会增加 `set_model_response` 工具，通过函数参数收集最后的结构化结果。[S12][S12a][S13] 官方文档指出这种 fallback 可能不可靠，提出另设格式化子代理的替代办法。[D2]

这里的结构验证仍需看类型：fallback 的 Pydantic model/list-of-model 分支会运行验证；其他 schema 类型的分支直接取 `response`。[S13] 即使类型全部验证通过，`{"conditionSatisfied": true}` 也只是一项模型输出，不能证明复现成功、下载完成或用户准许本机操作。

`PlanReActPlanner` 添加自然语言规划格式，并保留第一组函数调用、处理规划与最终答案标签。[S14] `BuiltInPlanner` 主要设置模型 thinking config。[S15] 两者都没有逐步检查计划所写的前置条件是否真的成立。结构化计划可以供调度、审计和恢复使用，执行合同须由程序另外维护。

## 条件与事实怎样落地

官方文档给出的条件阻断例子是应用回调读取 `tool_context.state['api_quota_exceeded']`，为真时返回错误。[D3] state 是应用读写的值与 delta；框架没有为任意键附带来源可信度、观察时间、对象版本或“事实已证实”性质。[S7]

新图工作流的 conditional edge 也只是匹配节点发出的 route 值。无 route 的边直接运行；匹配 route 的边运行，没有匹配则使用 DEFAULT 或结束该分支。[S16][D4] 如果 route 是模型根据未经核查的前提发出的，图仍然不会核查这个前提。图能够固定“先观察，再执行”的程序顺序，但不会把模型猜测变成工具事实。

对 KYNXA，条件至少应区分三项：用户规定的条件、模型认为需要检查的条件、工具观察到的当前状态。只有可验证的具体状态才进入执行合同，例如目标文件版本、下载任务状态、当前页面引用或测试执行回执；还须绑定相应对象和有效范围。“论文能通过复现”若尚无复现回执，当前仅介绍条件就不应产生打开浏览器动作。不能凭正则命中 `如果` 判定完成，也不能因为一句话含条件就删除其中所有独立肯定操作。

## 确认恢复与历史范围

`require_confirmation` 接受布尔值或调用参数驱动的函数。官方例子是报销金额大于 1000 时要求确认；这个数字条件由应用代码计算。[D5] 高级确认可请求 `hint/payload`，payload 的业务含义由工具应用解释。上游请假例子拒绝 `confirmed=false`，并将批准天数限制为原请求的天数，避免回执扩大请求。[S17]

生成确认事件时，框架分配新的 confirmation call ID，并保存 `originalFunctionCall` 的 ID、名字、参数。[S18] 恢复时读取当前 branch 的最近用户确认回执，映射回原工具调用；核对原调用存在、agent 作者、当前注册工具、确认要求及名字和参数一致，再执行原调用。[S19][S19a] 已消费的确认回执先去重，重复提交不会再次直接执行。上游测试覆盖未确认不运行、拒绝不运行、参数篡改和消费后的再次恢复。[T2][T2a][T2b]

这条链路没有把文本 `retry` 当作用户已批准，也没有将 model reason/confidence 当作确认回执。确认 UI/API 的调用身份合同可以借鉴；不应因此给 KYNXA 所有本来已授权的低影响动作增加审批。确认与条件证实还是两件事：批准打开浏览器，不代表下载已经完成。

确认能力仍标为实验性。此次官方文档还列出 DatabaseSessionService、VertexAiSessionService 不支持该能力，并要求启用 resume 时回传原 invocation ID。[D7] 因此不能凭确认恢复源码就承诺所有存储后端的持久恢复行为；集成时必须核对具体版本和后端。

ADK 的 Event 保存 author、invocation ID、branch 等；模型内容投影按 branch、逻辑隔离等筛选。`include_contents='none'` 仍保留当前用户输入和当前轮工具调用/结果，不是清空当前执行上下文。[S20][S21] 状态前缀区分 session、user、app 和单次 invocation 的 `temp:`；临时 state 不持久化。[S7][S22][D6] `temp:` 并不自动区分同一次 invocation 中的每个子任务，也不自动清除“浏览器能力已经不再适用”这种语义事实。

Event 的 `isolation_scope` 明确标为内部机制，不能当成应直接复制调用的稳定公开接口。[S20] KYNXA 可自建稳定 task ID/revision 和事件合同，借其隔离原则，不照搬内部字段。

## 对 KYNXA 已观测残差的建议

本地固定 58 题的最终结果是 42 条符合、13 处语义误放行、3 处外语肯定请求阻断；另有负事实讨论后 `retry` 继承旧操作的定向残差。[K1] 这些是纯函数/闸门结果，没有测量主模型选择；ADK 源码研究也没有提供能直接改写这组数字的实验证据。

| 残差 | 建议接口 | 可验证的部分与边界 |
| --- | --- | --- |
| 后置否定、无标记转述、双重否定讨论、外语肯定请求 | 当前主模型继续看原话、最近任务状态和能力描述，提出动作或继续解释。弱词法线索只供候选排序。 | 引用原文的位置可核对；引用存在不能证明模型解释正确。不能以几种语言动词正则作为统一允许/拒绝依据。 |
| 假设与未来条件误放行 | 执行前检查显式动作的具体前置对象和观察回执；缺少必要状态则先观察或保持待满足。 | 真正执行的测试/下载/页面结果可核对。模型自己写的 `conditionSatisfied` 不算证据。 |
| 解释负事实后 retry 复活旧操作 | 保存动作生命周期、目标和任务 revision，retry 只恢复仍有效的具体调用或任务；原始讨论历史继续保留。 | 核对 task/call/status/revision 和当前权限，不扫描旧浏览器动词重建允许。任务关系的自然语言理解仍交主模型。 |
| 禁新窗被扩大、整软件禁令被缩小 | action/target/effect 分开：读现有标签、激活窗口、创建窗口和禁用整个 app 是不同约束。 | MCP 连接可见仅证明有能力，不承诺具体动作满足限制；执行适配器需检查动作参数及现实对象。 |

建议首先借用“调用前合同检查 + 调用后事实更新”，连接 KYNXA 已有参数、权限快照、sourceRef/版本与动作回执。结构化动作提案可包含工具名、目标引用、用户消息引用、任务 ID/revision、需要的观察结果；它是待验证的数据，不新增一个 `authorizedByModel` 布尔值。模型的 reason/confidence 留作解释或诊断，不进入权限来源。

涉及用户意图的语义解释与程序事实验证必须分别报告。程序能检查原文 span、工具返回、版本和权限是否存在，不能用这些字段单独证明一句复杂中文授权了某动作。即使逐项存在，模型仍可能误解；真实模型评估、执行前合同和现有审批策略共同承担这项残余风险，不能声称增加结构字段就彻底解决。

### 模型轮次和等待代价

| 方案 | 额外模型轮次 | 实际代价与限制 |
| --- | --- | --- |
| 当前主模型直接提出工具调用，程序验证合同 | 通常 0 | 增加少量描述与引用字段；本地检查有开销，未实测时延。 |
| 先用一个模型生成结构化计划，再让主模型执行 | 至少 1 次串行模型调用 | 增加输入/输出 token、网络等待和模型生成；计划仍可能语义误判，不能当权限证明。 |
| 独立格式化子代理 | 通常至少 1 次模型调用 | 与官方 fallback 建议对应，但只改善输出接口，不保证当前动作适用。 |
| `set_model_response` fallback | 不强制独立分类调用 | 使用主模型最后的工具调用，schema 错误时可能需额外纠正轮次；不是每题固定多一次。 |
| 本地 before/after 回调或 route 检查 | 0 | 根据实现产生本地 I/O；不要用回调隐藏一个额外模型分类请求。 |
| 必须新取证的工具观察 | 可能多 1 个工具阶段及后续模型轮次 | 等待由实际观察工具决定；只查询会改变执行结果的缺口，不重复查已有有效回执。 |
| 真正需要的用户确认 | 不必增加一次意图分类调用 | 增加用户等待；恢复可直接关联已冻结调用，随后按需继续主模型。 |

可先评估同一次主模型选择和程序合同的方案，再单独对照是否值得增加结构化规划模型。固定题目、模型、预算并分别记录候选覆盖、主模型选择、参数/条件拒绝、真实执行、恢复结果、模型轮次与耗时。现有留出集已见，后续需增加新的独立表达；不能只让当前 16 题分数提高后就声称通用歧义解决。

## 一手链接

[S1]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/tools/base_toolset.py#L43-L59
[S1a]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/tools/base_toolset.py#L121-L151
[S2]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/tools/mcp_tool/mcp_toolset.py#L524-L552
[S3]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/tools/_agent_tools.py#L41-L127
[S4]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/base_llm_flow.py#L521-L542
[S5]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/tools/_caller.py#L508-L616
[S6]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/tools/_confirmation_utils.py#L25-L72
[S7]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/sessions/state.py#L72-L143
[S8]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/tools/skill_toolset.py#L2130
[S9]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/utils/_callback_pipeline.py#L81-L123
[S10]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/tools/function_tool.py#L167-L194
[S10a]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/tools/function_tool.py#L357-L372
[S11]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/features/_feature_registry.py#L175-L177
[S12]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/prompt/_schema.py#L34-L68
[S12a]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/basic.py#L129-L139
[S13]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/tools/set_model_response_tool.py#L281-L330
[S14]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/planners/plan_re_act_planner.py#L35-L85
[S15]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/planners/built_in_planner.py#L32-L86
[S16]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/workflow/_graph.py#L138-L188
[S17]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/contributing/samples/hitl/human_tool_confirmation/agent.py#L29-L74
[S18]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/tools/_functions.py#L206-L240
[S19]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/tools/_confirmation.py#L73-L218
[S19a]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/tools/_confirmation.py#L265-L402
[S20]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/events/event.py#L104-L147
[S21]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/flows/llm_flows/context/_contents.py#L624-L716
[S22]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/src/google/adk/sessions/_session_util.py#L51-L68
[D1]: https://github.com/google/adk-docs/blob/57e34aae019b97da72b6d739f5842781bca3278c/docs/callbacks/types-of-callbacks.md#L257-L274
[D2]: https://github.com/google/adk-docs/blob/57e34aae019b97da72b6d739f5842781bca3278c/docs/agents/llm-agents.md#L469-L499
[D3]: https://github.com/google/adk-docs/blob/57e34aae019b97da72b6d739f5842781bca3278c/docs/callbacks/design-patterns-and-best-practices.md#L85-L97
[D4]: https://github.com/google/adk-docs/blob/57e34aae019b97da72b6d739f5842781bca3278c/docs/graphs/routes.md#L7-L35
[D5]: https://github.com/google/adk-docs/blob/57e34aae019b97da72b6d739f5842781bca3278c/docs/tools-custom/confirmation.md#L128-L145
[D6]: https://github.com/google/adk-docs/blob/57e34aae019b97da72b6d739f5842781bca3278c/docs/sessions/state.md#L41-L78
[D7]: https://github.com/google/adk-docs/blob/57e34aae019b97da72b6d739f5842781bca3278c/docs/tools-custom/confirmation.md#L400-L430
[T1]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/tests/unittests/tools/test_function_tool.py#L820-L832
[T2]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/tests/unittests/flows/llm_flows/tools/test_confirmation.py#L1376-L1414
[T2a]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/tests/unittests/flows/llm_flows/tools/test_confirmation.py#L899-L926
[T2b]: https://github.com/google/adk-python/blob/128faabb6dddacf26e029aa8c28c903c3a7a6b61/tests/unittests/flows/llm_flows/tools/test_confirmation.py#L1135-L1213
[K1]: ./tool-ambiguity-holdout-20261009.md
