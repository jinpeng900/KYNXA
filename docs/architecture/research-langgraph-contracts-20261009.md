# LangGraph：状态、工具执行与恢复合同核查

核查日期：2026-10-09。只读研究作者源码和作者文档；未修改生产代码、测试或固定题预期，未调用模型或操作浏览器。以下“已核实”指指定提交中的公开实现，不推测采用者或厂商内部意图算法。

LangGraph 提供的是可组合的状态、执行与恢复机制。它不会自动把多语言原话解释成已授权动作，也不会替应用证明“如果……就……”的条件已成立。KYNXA 可以借用它的任务状态与回执组织方式，但换框架不会消除现存的 16 条工具题偏差。

## 固定来源与读取范围

| 作者项目 | 固定提交 | 本轮读取 |
| --- | --- | --- |
| `langchain-ai/langgraph` | [`bfcfea554ed5c7f7be562cebf8825e911b493ab1`](https://github.com/langchain-ai/langgraph/tree/bfcfea554ed5c7f7be562cebf8825e911b493ab1)，提交时间 2026-10-08 16:10:55 -0700 | Python `StateGraph`、消息 reducer、`ToolNode`、`interrupt`、内存 checkpointer、functional task 示例及相关上游测试 |
| `langchain-ai/docs` | [`be3028f3b446d7cfc63b434faf4e594689129251`](https://github.com/langchain-ai/docs/tree/be3028f3b446d7cfc63b434faf4e594689129251)，提交时间 2026-10-08 13:41:43 -0700 | Graph API、interrupts、checkpointers、time travel、消息维护、LangChain HITL 官方文档源文件 |
| `langchain-ai/langchain` | [`34489d61433cbf5044a630de2e7797e361672900`](https://github.com/langchain-ai/langchain/tree/34489d61433cbf5044a630de2e7797e361672900) | 当前 Python `HumanInTheLoopMiddleware` 源码，补核对 approve/edit/reject/respond 的实际行为 |

上述 HEAD 经 `git ls-remote` 获取；LangGraph 和 docs 浅克隆仅放在 `/tmp`，LangChain 单文件从固定提交读取。`docs.langchain.com` 请求返回 403，因而读取同一作者维护的 GitHub 文档源文件。未绕过环境代理，也未将第三方源码复制进项目。Node.js/TypeScript KYNXA 只借用合同思想；本轮没有审核 LangGraph.js 全套实现，不声称 Python 与 JS 每个细节相同。

## 已核实：结构化状态并不自行理解任务

[`StateGraph`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/langgraph/langgraph/graph/state.py#L131) 定义节点 `State -> Partial<State>`；每个字段可配置 reducer。未配置 reducer 的字段使用 [`LastValue`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/langgraph/langgraph/graph/state.py#L1850) 保存最近值；同一步多个写入会产生冲突。自定义 reducer 决定追加、合并或替换什么。

作者的 [Graph API 文档](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/graph-api.mdx#L88) 区分 `TypedDict`、dataclass 和可提供数据校验的 Pydantic schema。字段有类型或经过 schema 校验，不代表其自然语言含义、用户授权或事实已验证。`compile()` 的图结构检查也不证明应用层条件成立。

这对 KYNXA 的直接含义是：可以显式保存任务、候选解释、待确认条件和工具回执，但“结构化”不能成为把模型猜测升级为事实的步骤。模型输出 `confidence: 0.99`、`reason: 用户想打开浏览器`，仍然只是模型解释。

## 已核实：模型提议调用，ToolNode 执行已有调用

[`tools_condition`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L1616) 只查看最后消息是否包含 `tool_calls`，然后返回 `tools` 或 `__end__`。它不是语言意图分类器。

[`ToolNode`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L630) 可接受消息状态、消息列表或直接工具调用列表。执行路径做以下工作：

- 查找已注册工具；名称不存在时返回带对应 `tool_call_id` 的错误结果。
- 注入开发者配置的 state、store、runtime，并调用工具；参数 schema 错误按配置处理。
- 普通错误可成为 `status="error"` 的 `ToolMessage`；interrupt 特殊异常继续向上抛出。
- 同一步的工具可以并行；需要先读取再修改等依赖时，开发者必须用图、包装器或工具合同安排顺序。
- 返回 `Command` 时可更新状态、改变后续路由；更新当前图的工具结果有调用与结果配对检查。

具体证据为 [执行与错误处理](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L930)、[可配置 wrapper](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L1031)、[注册名称检查](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L1302) 和 [结果配对](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L1579)。

在该通用执行路径中，没有自动验证“原话是在翻译引文还是要求执行”“假设是否已成为当前决定”“下载完成或复现成功是否已证实”的步骤。`InjectedState` 只是提供状态，应用自己的工具/wrapper 必须读取并校验需要的前置条件。不同应用完全可以增加这些合同，但不能把可扩展性写成框架默认已实现。

## 已核实：interrupt 是暂停点，审批由应用定义

[`interrupt`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/langgraph/langgraph/types.py#L903) 暂停节点并向客户端提供载荷；恢复使用 `Command(resume=...)`。需要 checkpointer 和同一 thread。它本身既可以问年龄，也可以请求审批，暂停值并不天然具有审批含义。

当前 Python 实现支持 typed `response_schema`：Pydantic、TypedDict、dataclass 会校验恢复数据；JSON Schema 字典只向客户端展示，不做恢复值校验。没有 schema 时直接传递恢复值。即使布尔型 `approved` 合法，也仍需应用确定由谁、对哪次调用、哪些参数、何种权限范围提交。恢复值格式校验不能替代身份和授权校验。

作者 [工具内 interrupt 示例](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/interrupts.mdx#L633) 在发送邮件前暂停，由工具代码检查 `action == "approve"` 后执行，亦可接收改过的参数。这展示了开发者实现审批的接入点，不能证明所有普通工具自动具有审批或条件检查。

上层 [LangChain HITL 文档](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langchain/human-in-the-loop.mdx#L7) 描述的是可配置 middleware：`interrupt_on` 按工具名称和可选 `when` 参数谓词决定是否暂停，不是默认替用户理解全部自然语言。未纳入配置的调用可自动通过该 middleware；这也不取消工具和应用自身的其他权限检查。

## 已核实：当前 HITL 的编辑、拒绝与反馈

当前 [`HumanInTheLoopMiddleware`](https://github.com/langchain-ai/langchain/blob/34489d61433cbf5044a630de2e7797e361672900/libs/langchain_v1/langchain/agents/middleware/human_in_the_loop.py#L427) 在模型输出后、工具执行前收集需要复核的调用。每个待复核 action 需要一项按顺序对应的 decision，并检查 decision 类型是否被该工具配置允许。

| 决定 | 当前实现效果 | 需要保留的边界 |
| --- | --- | --- |
| `approve` | 保留提议调用，继续执行路径 | 仅处理这次待复核 action；不表示所有未来相似调用都获授权 |
| `edit` | 保存模型原调用；按调用 ID 在执行 wrapper 中替换成 reviewer 的调用 | 更改工具名会检查实际可用工具；执行路径仍应校验实际新参数 |
| `reject` | 生成 `status="error"` 的工具结果，说明没有执行 | 默认反馈要求没有新明确要求时别重试原调用；仍是给模型的行为指导，通用取消状态需应用维护 |
| `respond` | 人的回答作为工具成功结果，跳过真正工具 | 适用于人本身提供工具答案；不能拿它表示拒绝副作用，否则模型看到成功 |

源码分别见 [decision 处理](https://github.com/langchain-ai/langchain/blob/34489d61433cbf5044a630de2e7797e361672900/libs/langchain_v1/langchain/agents/middleware/human_in_the_loop.py#L345)、[edit 执行替换](https://github.com/langchain-ai/langchain/blob/34489d61433cbf5044a630de2e7797e361672900/libs/langchain_v1/langchain/agents/middleware/human_in_the_loop.py#L551) 和 [wrapper](https://github.com/langchain-ai/langchain/blob/34489d61433cbf5044a630de2e7797e361672900/libs/langchain_v1/langchain/agents/middleware/human_in_the_loop.py#L644)。

该 HEAD 特别给编辑后的真实结果附加说明：reviewer 替换了调用、实际执行了什么、不要重新发出模型原调用。新一轮会清空旧 edit 映射，防止其影响新的调用。这是 KYNXA 可以参考的原提议与实际执行分离。作者文档同时 [提醒](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langchain/human-in-the-loop.mdx#L497)：大幅编辑参数可能使模型重新评估、重复调用或采取意料外动作；结果提示不提供“绝不重复”的保证。

## 已核实：恢复与重放的边界

普通 interrupt 恢复会从包含 interrupt 的节点开头重新执行，interrupt 前面的代码也会重跑。多个 interrupt 的恢复值按节点内顺序匹配，任务之间的恢复值列表相互隔离；改动顺序或非确定性分支可能错配。作者 [规则](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/interrupts.mdx#L1001) 因而要求不吞掉特殊异常、不改变暂停点顺序，并 [约束暂停前副作用](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/interrupts.mdx#L1352)：幂等、放到暂停后或拆到独立节点。

Functional API 的 [task 示例](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/langgraph/langgraph/func/__init__.py#L322) 展示：已保存结果的 compose task 在同一暂停恢复中不会重复执行。这不是任意普通函数的自动 exactly-once 保证；外部副作用已发生而结果尚未成功保存的窗口仍需执行器的幂等标识或状态核验。

作者 [checkpointer 文档](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/checkpointers.mdx#L20) 说明成功 sibling 节点的 pending writes 可以避免在同一 super-step 故障恢复时重跑。其 [durability](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/checkpointers.mdx#L581) 三模式具有不同崩溃窗口；`sync` 增加保存开销，并不把外部 API 事务也自动纳入检查点事务。

这里的恢复单位是节点/task，不能随意等同于每次外部工具副作用。普通 `ToolNode` 的 [同步/异步执行](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L802) 在同一节点中执行一组调用并合并输出；如果中途 interrupt，节点整体重入仍会走这组调用。已有成功副作用能否避免重复，取决于应用把调用拆成何种 task、是否保存回执或使用幂等 wrapper，不能由“框架支持 pending writes”直接推定。本轮未运行此并行副作用场景。

从旧检查点进行 time travel 与正常暂停恢复不同。[重放文档](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/use-time-travel.mdx#L9) 明确写道：检查点后的节点重新执行，包括模型调用、API 请求和 interrupt，结果可能不同。`update_state` 创建分支检查点，不回滚旧执行历史；通过 reducers 应用更新并由指定节点的后继继续。重放会重新触发 interrupt，不可用旧“已批准”概述代替新调用检查。

## 已核实：thread 和 messages 不等于当前任务

[`thread_id`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/langgraph/langgraph/graph/state.py#L1204) 是检查点存取指针。复用它累积状态，使用不同 ID 则隔离独立状态；内存 checkpointer 以 [`thread_id/checkpoint_ns/checkpoint_id`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/checkpoint/langgraph/checkpoint/memory/__init__.py#L240) 查找快照。这些字段本身不识别换题、不撤销旧授权，也不能作为用户身份校验。多用户和跨项目可见性必须由应用另行约束。

[`add_messages`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/langgraph/langgraph/graph/message.py#L60) 根据消息 ID 追加或替换；`RemoveMessage` 删除指定消息，`REMOVE_ALL_MESSAGES` 清空当时列表后应用后续新消息。这是状态投影机制，不是不可改写原始审计日志。作者 [消息删除文档](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/add-memory.mdx#L2021) 还要求保留供应商合法的调用/结果序列。

`ToolNode` 的 [`_parse_input`](https://github.com/langchain-ai/langgraph/blob/bfcfea554ed5c7f7be562cebf8825e911b493ab1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L1259) 从消息列表向后找最近的 AIMessage 并读取其调用，职责是执行既有提议；它不会先审查更晚用户消息是否已纠正或取消。正常 agent 应由编排安排当前提议进入执行节点，不能直接把整段历史交给执行器当任务判断。

合并 reducer 收到空列表/空字典通常不会清掉旧字段。作者 [明确说明](https://github.com/langchain-ai/docs/blob/be3028f3b446d7cfc63b434faf4e594689129251/src/oss/langgraph/graph-api.mdx#L371) 需要替换 reducer 或 `Overwrite` 才能清空。因此“换题时返回空候选集合”若落在追加型 reducer 上，反而可能保留旧目标和旧限制。清掉 messages 也不会顺便清掉另一个 task/constraints 字段。

## 对 KYNXA 的建议，尚未实施

本节是设计建议，不是 LangGraph 已替 KYNXA 实现的功能。现有 KYNXA 已有请求身份绑定审批、执行回执、未知副作用停止、来源版本检查与完成验证，借鉴重点应是扩充它们的任务状态，保持 Node.js 链路；没有证据要求迁移整个应用到 LangGraph。

### 让原话、语义候选和执行合同各有来源

原始 JSONL 保持正式来源。主模型在现有决策轮继续看到当前原话和相关历史，解释“讨论/引文/条件/当前执行”“继续/纠正/换题”，并选择回答、观察或提议动作。若需结构化输出，随同一次主模型工具提议提供任务关系和来源消息引用，作为可检查的候选解释，不增加每轮隐藏的优化模型。

建议的状态字段包括 `taskId/taskRevision`、当前用户消息 ID、原始目标引用、模型候选解释、动作约束对象、待核验条件、已完成/已拒绝调用和回执引用。控制器持有当前任务版本和真实执行结果；模型不能自填 `authorized=true`、`conditionVerified=true` 或伪造回执。来源引用可验证存在、版本与范围；引用到一句真的原话仍不能在程序层证明模型对该句的语义理解正确。

工具目录继续支持搜索/加载，词法和多语言表达用于候选排序。中英词表不再适合作为未来通用“是否真的要求操作”的最终证明；移除现有硬闸之前须先评估替代执行合同和安全边界，不能简单改成信任模型自报许可。

### 把条件的讨论和已授权条件动作分开

“假如打开 Chrome 会怎样”由主模型直接解释；无工具动作。“如果下载结束就打开，现在介绍要求”保留为未生效条件和当前说明任务，不能因为出现打开动词执行。“如果浏览器未运行就启动”可以先用已有允许的观察检查状态，再决定已授权条件动作。

程序验证已定义、可观察的前置条件：引用确实来自当前观察，版本仍有效，连接/资源/参数合法，作用域和既有权限策略满足。可复现测试通过应来自实际执行回执，不能由模型写一段“复现成功”替代。没有可验证接口的条件保留未知；主模型按任务需要取证或说明缺口，只有会改变执行且确实无法从上下文/工具确定的信息才需要用户补充。

### 当前任务结束后，retry 不遍历旧关键词找动作

维护当前任务或当前待处理调用的稳定引用，而不是从整条 thread 向后找一个带浏览器动词的旧消息。换题/纠正的候选关系由主模型结合原话判断；显式取消事件和真实审批拒绝则记入任务状态。任务关系记录不能自动携带旧审批、路径参数或来源版本。

“不是所有浏览器都会恢复标签，解释这个事实”属于需要保留的否定事实。保留它给主模型；不把“恢复”变成当前操作，也不把整个句子丢掉。随后 `retry` 应继续最近的解释任务；如果当前没有可恢复的动作引用，就不能复活更早的打开 Chrome。新明确肯定请求可以建立新的动作提议，再走现有合同。

限制记录应具有动作、资源、执行环境和任务范围，例如“不要新窗口”只约束创建，“不要使用 Chrome”约束该软件，“改用云端”约束执行环境。模型提出约束解释时保留证据原文；控制器只执行它能验证的对象合同。不用一个 app 黑名单承载所有窗口限制，也不让模型解释文本直接成为权限令牌。

### 拒绝、编辑、未知结果分别恢复

借用当前 HITL 的原提议/真实执行分离：拒绝回执明确 `executed=false` 和原因，不能写成成功；编辑保留原提议、reviewer 改动、真实调用和回执，不静默改写原历史；新任务清除旧编辑映射。

实际拒绝、取消或执行未知时，记录当前调用与任务版本。停止同一个动作的自动重复；重新明确请求、真实状态核验或必要的政策批准再进入对应路径。不能以换一个 `tool_call_id` 当作新授权，也不能以整体重放掩盖“上次可能已经执行”。若未来增加检查点，应围绕副作用/回执保存窗口设计幂等键和恢复核验；本轮只读研究不能声称 KYNXA 已具备 LangGraph 持久恢复。

## 固定 58 题残差的具体去向

依据 [当前最终探查报告](tool-ambiguity-holdout-20261009.md)，最终 42/58 符合，13 处语义误放行、3 处外语请求阻断；这些是纯函数探查结果，不能当成真实主模型的误执行率。

| 残差 | 数量 | 设计应该验证什么 |
| --- | --- | --- |
| 后置/内嵌否定 `neg-01/02/05` | 3 | 主模型保留原话并识别当前解释目标，程序不把动词品牌当许可证明；新肯定对照不能被一并封禁 |
| 无引号转述 `quote-06` | 1 | 分清转述建议与当前要求；引文来源不授予执行权限 |
| 假设与未来条件 `cond-01…06` | 6 | 分清说明、条件动作和现在执行；可观察条件需要真实证据，另保留合法条件执行反向题 |
| 允许性/双重否定讨论 `double-04/05` | 2 | “不禁止”与“现在要求”分别表达；保留讨论内容而不启动软件 |
| 西/德/法明确请求 `mixed-04…06` | 3 | 用同一主模型解释外语，检查目录可见性与实际合同，测出硬词法阻断；不能靠多贴三份词表宣称通用多语言 |
| 只解释读取方法 `window-07` | 1 | 区分能力说明和实际读取；新窗口禁令不能成为读取操作的证据 |
| 固定集外 negative-fact → `retry` | 不并入 58 题 | 保留否定事实、最近任务与取消状态，停止更早浏览器动作继承；另测独立肯定操作仍可执行 |

冻结题已被用于开发，今后作为回归保留；主模型方案要另外补独立表达。比较保持模型、题目、工具预算和权限模式不变，分别比较提示组织、目录策略、任务状态或执行合同。分项记录：能力是否可见、模型提议、参数是否合法、是否执行、拒绝原因、恢复是否重复和延迟。不能只报告 intent 布尔值或把框架换名算成改善。

## 模型轮次与时延成本

以下是调用结构的成本推导，未进行联网模型基准，不能给出实测秒数或准确率。

| 路径 | 相对现有主模型决策的额外轮次 | 成本来源 |
| --- | --- | --- |
| 原话/相关状态随现有请求提供，模型同轮回答或提议工具 | 0 | 多出的上下文/输出 token、状态校验与保存 |
| 同轮携带结构化候选关系及提议，控制器直接校验 | 0 | schema 适配与更长输出；候选元数据不构成授权 |
| 每轮单独加“意图优化/翻译模型” | 通常 +1 | 新模型往返与输入输出；本研究不建议默认增加 |
| 未知条件需要先做一次观察，再由主模型决定 | 通常 +1，取决于已有循环 | 观察工具耗时及下一次主模型决策；若本来就需要观察，这属于正确执行的必要成本 |
| 收到可恢复的合同失败后重新选择 | 每次恢复通常 +1 | 错误回执后的模型决策；应有上限与停止条件 |
| 真实审批暂停/恢复 | 审批机制本身不必增加模型轮次 | 用户等待、检查点 I/O、节点重执行；恢复路径若包含模型则按图结构实际增加 |
| 从旧检查点重放 | 取决于检查点后的模型节点数量 | 后续模型、工具与 interrupt 重新发生；不能按“恢复=零成本”估算 |

建议测 `modelRounds`、prompt/output tokens、实际工具次数、重复动作数、p50/p95 请求时延、单独的审批等待、来源回读和检查点耗时。KYNXA 现有工具循环已记录模型轮次、工具时长和审批等待，可在隔离评估中扩充任务关系/前置条件/真实调用日志。程序校验不应再隐藏一次模型请求；额外取证也不能为了省一轮而被省略。

## 本轮验证范围

来源提交与上述路径经本地源码读取核对，相关上游测试仅阅读，未宣称本环境运行了其测试。未改动生产或固定题预期，未做实际模型质量/时延测量；具体改造应由 KYNXA 现有领域负责人按上述分项评估后实现。
