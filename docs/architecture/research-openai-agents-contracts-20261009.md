# OpenAI Agents SDK：可见能力、模型动作与执行合同

研究日期：2026-10-09。只核查作者公开源码，没有运行模型或第三方工具，也没有改动 KYNXA 生产代码。固定版本为 [125efa029b4bfd84238bd2c4fd69c3406f802663](https://github.com/openai/openai-agents-python/tree/125efa029b4bfd84238bd2c4fd69c3406f802663)，作者提交时间为 2026-10-08T19:29:11Z；以下链接不使用可变 `main`。

## 源码确认的事实

| 界面 | 实现事实 | 不能据此声称的能力 |
| --- | --- | --- |
| 候选能力 | [`Agent.get_all_tools`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/agent.py#L285) 合并 MCP 与注册工具，并执行应用提供的 `FunctionTool.is_enabled` 回调；这是可见性控制。 | 框架自动判断中文歧义、用户当前意图或某个动作已经获准。 |
| 动态发现 | [作者工具说明](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/docs/tools.md#L71) 的 `ToolSearchTool`、`defer_loading`、namespace 供 Responses 模型按需加载候选。client-executed search 不由标准 Runner 自动执行。 | 所有协议都支持同一 hosted search；检索排名等于最终工具选择。 |
| 模型行动 | [`Runner.run`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/run.py#L291) 的循环先调用模型，模型发工具调用后才运行工具并回送结果。 | 排名首位工具被程序自动执行。 |
| 结构化输出 | [`Agent.output_type`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/agent.py#L383) 与 [`AgentOutputSchema`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/agent_output.py#L72) 定义、解析和验证 JSON 类型。 | `condition_met: true`、`authorized: true` 等字段因此成为真实事实或用户授权。 |
| 应用状态 | [本地 context](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/docs/context.md#L26) 默认不发送给模型；应用可在回调、工具中使用。 | 模型天然知道该状态；框架自动验证其中的事实。 |
| 参数合同 | [`function_tool` 准备与调用](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/tool.py#L2761) 解析 JSON、通过生成的 Pydantic 参数模型并确认准备参数属于同一调用；再调用实际函数。 | 合法 JSON 说明目标存在、版本当前、某个自然语言条件成立。 |
| 调用前验证 | [`_execute_single_tool_body`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/run_internal/tool_execution.py#L2015) 在函数体前执行 tool-input guardrails；[`_execute_tool_input_guardrails`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/run_internal/tool_execution.py#L2732) 消费应用回调返回的 allow、reject_content 或异常。 | 默认内置了“引语、否定、假设、前置条件”的完整语义检查器。回调可以是确定性检查，也可以额外调用模型，后者另计成本。 |
| 审批 | [`FunctionTool.needs_approval`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/tool.py#L499)、[审批流程](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/docs/human_in_the_loop.md#L49) 和 [调用绑定检查](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/run_internal/tool_execution.py#L1872) 处理 call ID、工具身份、参数与已保存决定。 | 模型的理由或 confidence 是审批；同名工具在另一服务上的决定可自动通用。 |
| 会话历史 | [session 合并说明](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/docs/sessions/index.md#L62) 与 [`RunConfig.session_input_callback`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/run_config.py#L446) 可过滤模型本轮历史，存储仍只追加本轮项目。 | 同一个 session 自动识别换题，或把旧操作许可转换成当前任务许可。 |

这些接口提供的是承载模型决策、应用状态和验证的结构，不是一个通用意图算法。作者示例或源码没有证明它能自动解决 KYNXA 的 58 条留出题。

## 容易误借的边界

Input guardrail 默认可以与 agent 并行。[作者说明](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/docs/guardrails.md#L31) 明确指出：拒绝到达前，模型可能已经消耗 token 或执行工具。需要阻止副作用的检查应在实际调用前同步完成，不能把一个并行分类器当作执行前保证。`run_in_parallel=False` 会先等 guardrail；若 guardrail 自身调用模型，就增加等待与费用。

Tool guardrail 的覆盖也有界。[固定版本说明](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/docs/guardrails.md#L71) 将该管线限定于 FunctionTool；hosted tools、ComputerTool、ShellTool、ApplyPatchTool 等走其他路径。KYNXA 若借用这一设计，必须明确每种执行器的实际验证入口，不能只装一个钩子就声称所有执行都受到保护。

[`ToolExecutionConfig.pre_approval_tool_input_guardrails`](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/src/agents/run_config.py#L142) 可以让函数工具检查先于审批中断；批准后仍立即再检查。这适合目标或来源可能变化的场景，也说明审批并不使之后的参数与环境验证失去必要性。

[服务器侧审批说明](https://github.com/openai/openai-agents-python/blob/125efa029b4bfd84238bd2c4fd69c3406f802663/docs/human_in_the_loop.md#L195) 明确区分状态反序列化与身份鉴别：RunState 恢复不认证提交者或快照，应用仍要认证所有者、保存服务器版本、绑定待审调用并防止重放。不能把“计划 JSON 合法”或“调用 hash 一致”当成权限来源。

## 对 KYNXA 的建议

建议借用同一主模型的结构化当前动作提议、本地可信状态、每次调用前验证与带身份的暂停/恢复；继续保留 KYNXA 三协议的 `tool.search`/`tool.load`，不要因为 Responses hosted search 看起来方便就替换其他协议的可用路径。

提议中的自然语言解释只能作为可复查的模型决定。已证实的前置条件应引用服务器拥有的工具回执，由 broker 校验对象、版本、范围和检查种类；权限仍来自当前真实权限模式、用户已给出的有效授权或既有批准记录。模型填写 `verified: true` 不生成证据，`reason` 与 `confidence` 不生成授权。

最小成本的接口是让实际 effect tool 调用携带小型提议元数据，由编排层剥离后按原工具 schema 校验。这样保持真实工具名称和候选描述，也可以在同一生成中完成语义决定，不需要新增一个每轮都运行的分类模型。它需要 KYNXA 自己设计 schema、预算和历史状态，不是导入 SDK 后自动具备的功能。

若改为先让模型输出完整计划，再开一个 executor agent 选择实际动作，通常增加一个串行模型阶段。若 tool-input guardrail 调用另一模型，也增加一次模型调用；确定性回调本身不增加模型轮次。不能在未做 API 延时实测前给出固定毫秒收益。

跨 ADK/LangGraph 的具体 KYNXA 合同建议与成本对照见 [综合设计](agent-action-contract-proposal-20261009.md)；本文件只记录已核查的 SDK 事实与其边界。
