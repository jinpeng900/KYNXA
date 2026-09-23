# KYNXA v1.4.2-R3
## 精炼与工程细化版

基于 2026-09-18 详细完整版修订 · 编辑日期 2026-09-23

文档状态：R3 冻结内容的编辑整理 + 待评审工程建议。不是新软件版本，也不是已实现能力清单。

## 阅读说明

本版保留 18 个专项和 5 个附录。精简对象是反复出现的通用原则、相同表格说明和模板段落；细化对象是会影响代码正确性、安全隔离及故障恢复的接口与状态。原始文档包不作覆盖。

规范优先级：00 全局工程契约 > R3 各专项冻结结论 > 原 R3 详细展开 > 旧版非冲突细节。每章新增 K 节以及附录新增细化条目均为建议设计，不自动进入冻结层；与冻结内容冲突时必须走 ADR。

阅读顺序：总体理解先读 01、03、06、09；实现前再读 00、17、附录 C/D；桌面开发读 02、07、附录 E。后续功能域保留产品范围、约束和验收，不要求全部进入首个 CodeRepair 切片。

### 本次实质调整

- 通用对象、并发、错误、观测和交付规则集中到 00.K，避免每个模块重新重复一遍。
- 补充 TaskNode/Grant/SideEffect 状态机、Session 幂等创建、事件补投与 crash-cut 恢复矩阵。
- 细化 IPC Envelope、消息族、数据库最小表集合、scope 一致性和事务边界。
- 明确独立进程与实际 OS 隔离的区别，并将未确定的技术选择列为 ADR。
- 区分代码现状、冻结要求和工程建议；不把示例连接诊断、性能数字或文档测试当成已验证结果。

### 阅读标识

“冻结决策”继承原 R3；“建议”用于设计评审和实现验证；“当前仓库”仅指本次检查到的 Desktop 原型；“验收”表示尚需执行的检查。DSH 上游 API 未在本轮编辑中联网核实，具体版本与导出接口见附录 B 的实施前清单。

## 章节导航

- 00 · Global Engineering Contract：全局工程契约、安全不变量与跨模块协议
- 01 · 总体系统架构与产品蓝图
- 02 · 产品功能需求与 GUI 交互详细设计
- 03 · Work、Conversation 与模型无关 CognitiveState
- 04 · 知识、记忆、经验与决策记忆系统
- 05 · Skill、Tool、MCP 与社区能力生态
- 06 · DeepSeek Harness 集成、持久任务编排与 Agent 执行内核
- 07 · 模型管理、本地模型运行时与异构消费级硬件自适应
- 08 · 能力模块：浏览器、计算机、文件、代码与 Artifact
- 09 · 安全架构、权限系统与 Full Access
- 10 · Resident、Android 远程控制、设备配对与统一审批中心
- 11 · Agent Wallet、购物 Skill 与 Agent Commerce
- 12 · Routine、Intent Subscription 与个人自动化
- 13 · 个人 Agent 体验、偏好学习、Shadow Mode 与 Agent Constitution
- 14 · 缓存、存储、快照、回放、可观测性与灾难恢复
- 15 · 研究价值、开源 Benchmark、实验评估与开发路线
- 16 · 网络架构、Web Search、VPN/代理、远程连接与实时同步
- 17 · 数据库、本地持久化、关系模型与索引详细设计
- Appendix A · R2 → R3 完整迁移矩阵
- Appendix B · DeepSeek Harness Upstream 映射与同步策略
- Appendix C · IPC Schema、数据对象与事件契约
- Appendix D · 验收、测试与 Benchmark 矩阵
- Appendix E · WinUI 3 · 1440×900 布局与组件基准

# 00 · Global Engineering Contract：全局工程契约、安全不变量与跨模块协议

## 文档目的
本文件是 R3 的最高工程契约。R3 不扩张模块范围，而是把 Agent 执行内核从“由 KYNXA 自研完整 Harness”收敛为“DeepSeek Harness（DSH）作为 less-trusted Agent Execution Engine，KYNXA 继续拥有产品状态、长期任务、知识、模型治理、资源治理与独立 Rust Authority”。任何子模块只能收紧本契约，不能通过 Prompt、插件、DSH Tool、MCP、Remote、Full Access 或用户自定义模型绕过。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| R3-CTR-001 | Intelligence proposes; Authority decides | LLM、DSH agent-loop、subagent、workflow、Skill、MCP、Web 内容只产生建议/工具意图，不拥有宿主高权限。 |
| R3-CTR-002 | DeepSeek Harness 是执行内核，不是产品真相源 | DSH Session/Workspace 可持久，但 Work/Task/CognitiveState/Policy/Artifact 的业务真相仍由 KYNXA 管理。 |
| R3-CTR-003 | Authority 独立 Rust 进程 | Release 禁止把受保护执行退化为 Host/Node/Python 内部函数。 |
| R3-CTR-004 | 范围冻结继续有效 | 不新增一级产品模块；R3 是集成重构。 |
| R3-CTR-005 | 模型与权限正交 | 模型强弱、Provider、DSH profile、Full Access 都不能扩大安全权限。 |
| R3-CTR-006 | 副作用可恢复但不假装 exactly-once | non-idempotent 行为采用 request binding、idempotency key、SideEffectRecord 与 reconciliation。 |

## 1. 可信域与进程边界
R3 将系统分成四类信任域：① UI/Product Host：WinUI 3 + C#，负责交互与业务协调；② Intelligence Plane：DSH、模型、RAG、Skill、MCP、网页与 Python specialized worker，全部按 less-trusted 处理；③ Trusted Authority Core：Rust 独立进程，负责 canonicalization、Policy、Grant、Credential、Device 与安全审计；④ Restricted Executors：只持有单次/限时/限资源的执行能力。DSH 自带的 permission/sandbox/approval 机制可以作为“执行前便利层”，但不能取代 KYNXA Authority 的最终决策。

## 2. Source of Truth 规则
业务状态、安全状态、文件字节、可重建索引与 Agent Session 必须明确分域。kynxa.db 保存 Work/Conversation/Task/CognitiveState/Artifact/ModelPolicy；authority.db 保存 Policy/Grant/Approval/CredentialRef/Device/Audit；Blob Store 保存用户文件与 Artifact 字节；Qdrant/FTS/Session Query 等索引均可重建；DSH Session JSONL 保存 agent turn/tool/model 事件事实。任何单一存储损坏都不得让其他域的安全语义被隐式推导。

## 3. 跨模块 Contract IDs
CapabilityRequest、ApprovalIntent、AuthorizationGrant、SideEffectRecord、TaskNodeExecution、ModelExecutionPlan、ContextBundle、SessionBinding、RemoteCommand 都必须携带稳定 ID、revision、scope 与 correlation_id。跨进程 IPC 不依赖“字符串里约定一下”；Schema 版本升级必须显式。

## 4. Fast / Agent / Deep 三条路径
普通问答走 Fast Chat Path：UI→Host→Context Lite→DSH/Model→Stream，不创建不必要 TaskGraph。需要工具时进入 Agent Work Path：Host 建立 Durable TaskNode，交由 DSH Session 执行。长任务进入 Deep Work Path：允许多个 TaskNode、Checkpoint、subagent/workflow、恢复与资源调度，但副作用仍由 Authority 串行约束。

## 5. 禁止事项
禁止 DSH 直接调用宿主 PowerShell/文件写/浏览器提交等高风险动作；禁止 Community Skill 通过自带脚本绕过 Capability Broker；禁止把 DSH Workspace 等同于 KYNXA Work；禁止将 Session JSONL 作为业务数据库；禁止 Python/Node 保存长期 API Key 明文；禁止恢复流程重复消费一次性 Grant。

## Definition of Done
- 所有高风险能力存在 Authority 测试
- 任何 Tool Call 可追踪到 Task/Session/Grant
- Kill/restart 后不会重复不可逆副作用
- Full Access 仍保留 canonicalization/audit/revoke
- R3 文档间术语与数据所有权一致

### 全局对象与版本规则
- 推荐 UUIDv7/ULID 作为跨进程可持久化 ID；显示名、路径、模型昵称不能承担身份语义。
- `schema_version` 表示结构兼容性，`revision` 表示同一对象的可变世代；审批、Context、缓存和 side effect 必须绑定相关 revision。
- 事件最少包含 `event_id / correlation_id / causation_id / timestamp / producer / schema_version / scope_ref`。
- 错误分为 Validation、Policy、Capability、Dependency、Transient、UnknownSideEffect、Internal 七类；只有 Transient 且满足幂等条件才允许自动重试。

### IPC Envelope
所有 Desktop↔Host、Host↔Agent Runtime、Host↔Authority、Host↔Worker 的跨进程消息使用共同 Envelope。Envelope 不允许把任意 JSON 字符串直接透传到高权限执行器；Authority 接口只接受固定 schema 的 CapabilityRequest/GrantClaim/SideEffectCommit。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | 跨模块术语、稳定 ID、revision/schema_version 规则 |
| OWN-02 | Trust Zone 与 less-trusted / trusted 边界 |
| OWN-03 | CapabilityRequest / ApprovalIntent / AuthorizationGrant / SideEffectRecord 的规范语义 |
| OWN-04 | IPC Envelope、correlation/causation、错误分类与幂等要求 |
| OWN-05 | Model Policy 与 Authority Policy 的正交关系 |

### A.2 明确不拥有
- 具体 GUI 布局
- 具体模型算法
- 具体 Skill SOP
- 某个 Provider 的私有实现

边界原则：**谁拥有事实，谁负责持久化与 revision；谁拥有授权，谁负责最终允许/拒绝；谁消费能力，不因此获得该能力的安全所有权。** 跨模块不得通过共享可变内存或“默认大家都知道”的隐式约定耦合。

## B. 核心对象目录

ContractId；ScopeRef；RevisionRef；CapabilityRequest；ApprovalIntent；AuthorizationGrant；SideEffectRecord；ModelExecutionPlan；EventEnvelope；CheckpointRef。

## C. 服务职责目录

Contract Registry；Schema Compatibility Gate；Scope Resolver；Revision Validator；Event Envelope Validator。

## D. 主流程与状态推进
1. 任何模块构造跨进程请求前先绑定 scope/revision/schema_version
2. 受保护动作 canonicalize 后形成 request_hash
3. Authority 只消费结构化请求，不消费模型自然语言作为授权事实
4. 副作用进入 PREPARED→SENT→CONFIRMED/UNKNOWN/FAILED ledger
5. 协议升级通过显式 Contract Migration，而不是局部绕过

每一步都应留下可诊断事件。可恢复流程必须区分“已计划、已授权、已 claim、已执行、已确认、状态未知”，不能用一个 boolean `done` 代替。若流程中含模型调用，模型输出只能生成候选意图/Observation，最终业务状态仍由 Host/Authority/Verifier 更新。

## E. 不变量与安全要求
- SEC-001：LLM/Planner/Router/Skill/DSH Runtime 不拥有 ambient host privilege
- SEC-002：Approval != Execution，审批结果不能替代 Grant 校验
- SEC-003：Credential 只允许 use-by-reference，不进入模型上下文与普通日志
- SEC-004：Graph/Policy/Scope/参数 revision 变化使旧批准失效
- SEC-005：non-idempotent side effect 在 UNKNOWN 状态禁止盲重试
- SEC-006：Web/Knowledge/Community Skill 均视为不可信数据
- SEC-007：Full Access 只能放宽交互频率，不关闭 Authority/Audit/Kill Switch

任何实现优化（缓存、并行、预取、自动批准、批量执行）都不得破坏这些不变量。若优化导致无法证明不变量，应默认关闭该优化，而不是在文档里用“通常安全”替代可验证条件。

## F. 并发、幂等与 Revision
- 写操作使用 optimistic revision 或单写者队列，拒绝 silent last-write-wins。
- 只读节点可并行；会改变外部世界或同一对象的写节点默认串行。
- 自动重试仅用于可证明幂等或尚未发送的步骤；外部副作用进入 UNKNOWN 后先 reconciliation。
- 用户审批、ContextBundle、ModelExecutionPlan、TaskGraph 均绑定 revision；任一关键 revision 变化，旧派生物进入 stale。
- 多设备/多进程同时操作同一对象时，使用稳定 ID + claim/lease/transaction，而不是依赖 UI 是否“看起来只有一个按钮”。

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | 协议版本不兼容→拒绝并返回 CONTRACT_VERSION_UNSUPPORTED |
| F-02 | scope 缺失/漂移→SCOPE_MISMATCH |
| F-03 | revision 过期→STALE_REVISION |
| F-04 | 重复副作用 claim→ALREADY_CLAIMED/RECONCILE |
| F-05 | Authority 不可用→Fail Closed |

降级必须被记录到用户可理解的 trace/status：例如模型降级、网络代理失效、索引重建、只读恢复模式。不得为了“任务继续”静默扩大 Cloud、权限、Work Scope 或副作用范围。

## H. 指标目录

contract_validation_failures_total；stale_revision_reject_total；approval_digest_mismatch_total；side_effect_unknown_total；scope_violation_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 修改审批后的 path/amount，旧 Grant 必须失效 |
| T-002 | 模拟 Python/Node Runtime RCE，不能越过 Authority 读取未授权用户目录 |
| T-003 | 同一 max_uses=1 Grant 在手机和 PC 并发 claim，只能成功一次 |
| T-004 | 支付网络超时进入 UNKNOWN 后只能 reconciliation |
| T-005 | Cloud=DENY 时任何 Skill/Router 都不能静默外传 Work 内容 |

测试至少包含：正常路径、用户拒绝、依赖超时、进程崩溃、重启恢复、并发、旧 revision、数据损坏/索引丢失、恶意输入。所有 P0 安全/恢复测试进入 CI release gate。

## J. 实施顺序
1. 先冻结 JSON Schema/IDL 与 Contract ID
2. 为 Host/Authority/Agent Runtime 建 shared conformance tests
3. 把旧散落错误码收敛到统一 taxonomy
4. 把 Contract tests 设为 release gate

每个里程碑的 Definition of Done 必须同时包含代码、Schema、Migration、自动测试、日志/metric、失败路径和最小用户 UI；只完成 happy path 不算模块完成。

## K. 统一工程规则与解释修正

本节集中承接原各章重复的对象、依赖、并发、失败、观测与交付要求。原冻结结论优先；本节新增的字段、数值和算法均为建议基线，进入代码前须通过契约评审。文档中的 MUST 表示拟议契约内的强制条件，不表示该方案已经获得产品冻结。

### K.1 通用对象与命名

稳定 ID 只承担身份语义；推荐统一选择一种 UUIDv7 表示，不混用路径、名称和数据库自增序号作为跨进程身份。事件 ID、业务操作 ID 与一次传输的 message_id 分开。重传更换 message_id，但保持 operation_id；有意发起一次新动作则创建新的 operation_id。

schema_version 表示序列化结构，revision 表示可变对象的版本。不可变事件使用 event_id 和 sequence，不强行给每条事件增加可变 revision。更新请求携带 expected_revision；不匹配时返回 STALE_REVISION，禁止静默覆盖。统一使用 UTC 时间记录事实，时区只参与展示和调度解释。

同一字段在 C#、TypeScript、Rust 中须具有一致的取值范围。建议跨语言计数器以十进制字符串传输，避免 JavaScript 整数精度差异；数据库可用有界整数，并拒绝溢出。金额以 currency + minor_units 整数表示，不能用浮点金额参与授权摘要。

### K.2 所有权、提交与一致性

业务对象由 Host 单一逻辑写者管理；安全事实由 Authority 独占。Desktop 不直写业务库，Agent Runtime 不直写任一真相库。服务名称表示模块职责，不意味着每个服务都拆成独立进程。只读节点可并行；同一对象或资源的写动作需协调，避免串行化所有无关工作。

业务状态更新、事件追加与 outbox 写入在同一个业务库事务中完成。跨进程投递允许重复，消费者依 event_id/operation_id 去重。跨 kynxa.db、authority.db、文件系统和外部服务不承诺原子提交，使用状态机、receipt 与 reconciliation 收敛。缓存键包含 scope、输入摘要、工具/模型版本；缓存命中不继承执行授权。

权限相关 revision 变化使旧批准失效；展示名称变化是否触发失效，应由明确的授权字段集合决定，不能依赖开发者猜测。审批摘要和执行请求都必须引用同一个版本集合。优化、缓存或恢复路径只能维持或收紧权限，不能静默扩大范围。

### K.3 错误分类修正

原 00 章写“六类”但实际列出七项；本版统一为七类。错误响应包含 code、category、safe_message、operation_id、retry_advice、diagnostic_ref；retry_advice 只描述建议，接收方仍要校验预算和副作用状态。

| 分类 | 典型 code | 调用方行为 |
|---|---|---|
| Validation | INVALID_PAYLOAD / CONTRACT_VERSION_UNSUPPORTED | 修正请求或升级；不原样重试 |
| Policy | POLICY_DENIED / STALE_APPROVAL | 停止该动作；按新请求重新评估 |
| Capability | CAPABILITY_UNAVAILABLE | 明确缺失能力；不得隐式换权限或模型 |
| Dependency | AUTHORITY_UNAVAILABLE / MODEL_UNAVAILABLE | 受保护动作 fail closed；显示依赖状态 |
| Transient | BUSY / RATE_LIMITED | 仅在幂等成立、预算允许时退避重试 |
| UnknownSideEffect | SIDE_EFFECT_UNKNOWN | 进入核实流程，不重复执行 |
| Internal | INTERNAL_ERROR | 保存诊断引用，避免泄露内部路径或 Secret |

拒绝批准是正常业务结果，不等于异常。超时、取消和连接断开都不证明动作未执行。FAILED 也不能笼统解释成“没有副作用”；部分写入、部分发送必须保留明细并进入核实或补偿流程。

### K.4 统一可观测性与发布门槛

事件贯穿 Desktop→Host→Runtime→Authority/Executor，使用 correlation_id 和 causation_id 关联。默认仅记录敏感 payload 的受控引用或摘要；不能认为哈希天然匿名，低熵 Secret 连哈希也不应进入普通日志。高基数 task_id/session_id 放 trace，不作为 metrics 标签。

每章列出的指标保留，但统一遵守：计数器单调累加；延迟记录直方图和 p50/p95/p99；成功率写明分母、时间窗和失败分类。恢复成功必须包括状态一致且无重复副作用，不能仅以进程重新启动计数。

模块交付同时具备实现、版本化契约、迁移、正常与失败路径测试、日志指标和必要 UI。P0 覆盖拒绝、超时、崩溃、重启、并发、旧 revision、损坏数据和恶意输入。未启用的后续模块不阻塞最小切片，但发布说明必须准确标出范围；已启用模块对应 P0 不得跳过。


# 01 · 总体系统架构与产品蓝图

## 文档目的
R3 的系统形态仍是 Windows-first、Local-first、GUI-first 的 Personal Agent Host，Android 是控制平面。变化只发生在 Agent Runtime：KYNXA 不再维护第二套完整 planner/tool-loop/subagent/workflow 栈，而是将 DeepSeek Harness 作为可组合执行内核，通过窄适配层接入 KYNXA State、Context、Model、Capability 与 Authority。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| ARCH-001 | Desktop 前端 | C# + WinUI 3 + XAML + MVVM；HTML 只用于原型，不作为主体。 |
| ARCH-002 | Host | kynxa-host.exe（.NET）拥有 Work/Task/CognitiveState/Event/Recovery/RemoteSession。 |
| ARCH-003 | Agent Runtime | kynxa-agent-runtime（Node/TypeScript）装配 DSH core/session/tools/skill/subagent/workflow/web 等。 |
| ARCH-004 | Authority | kynxa-authority.exe（Rust）独立。 |
| ARCH-005 | Model Runtime | KYNXA Model Runtime 管理本地模型生命周期与资源；云/自定义 Provider 通过统一 Model Adapter 暴露给 DSH。 |
| ARCH-006 | Python 定位 | 仅保留 embedding/rerank/parser/OCR/CV/ML 等 specialized worker，不再作为中央 Orchestrator。 |

## 1. 进程拓扑
桌面 UI 只处理视觉状态与命令；Host 将普通 Chat 与 Work 统一建模。需要 Agent 能力时，Host 创建/绑定 DSH Session 并发送 ContextBundle。DSH 负责 agent loop、tool registry、skill、subagent、workflow、compaction；所有有副作用 Tool 通过 KYNXA Tool Bridge 转换为 CapabilityRequest；Authority 返回 DENIED/APPROVAL_REQUIRED/GRANT；Restricted Executor 执行并将 Observation 回写 DSH，同时 Host 将可恢复事实归并到 CognitiveState。

## 2. 组件责任边界
KYNXA 负责“用户产品语义”和“长期真相”：Work、Child Work、Conversation、Task、Knowledge、Library、Artifact、Model Management、Resource Scheduler、Remote、Policy。DSH 负责“单个 Agent Session 内如何执行”：模型轮次、工具调用、上下文压缩、subagent、workflow、Web 工具表面。两者通过 SessionBinding 与 Event Bridge 连接。

## 3. 生命周期
App 启动：Resident/Authority 优先；Host 加载 kynxa.db 并恢复未完成任务；Agent Runtime 按需启动并装配 profile；Model Runtime 仅在需要时加载模型。普通 Chat 不应被强制等待完整 DSH 大型 profile。Agent Runtime 崩溃时 Host 保持业务状态，使用 Session Persistence + Checkpoint 再绑定。

## 4. 模块冻结
一级产品域继续保持 Chat、Work、Library、Knowledge、Models、Skills、Remote、Scheduled Tasks、Inbox/Approval、Settings；Browser/Code/File/Computer/Artifact 属于 Capability，不升级成新的一级产品。Shopping/Wallet/Shadow/Preference 等既有设计保留但不是当前 Vertical Slice 阻塞项。

## 5. 首个 Vertical Slice
CodeRepair：创建 Work→选择本地/云 Coding Model→挂载 repository→DSH 分析/调用只读工具→Authority 允许 sandbox build→产生补丁候选→用户/Policy 批准写入→tests/verifier→checkpoint→故意 kill Host/Agent Runtime→恢复→生成 diff/Artifact。该 Slice 必须证明 DSH 可替代旧自研 loop，而 Authority、Work 与恢复语义没有退化。

## Definition of Done
- 进程可独立启动/崩溃恢复
- 普通 Chat TTFT 不受完整 Agent Runtime 固定开销影响
- CodeRepair Vertical Slice 全链路可跑
- DSH 升级可通过 adapter 回归而非全项目重写

### 推荐进程拓扑
`kynxa-desktop.exe` 只负责 WinUI 3 展示与用户输入；`kynxa-host.exe` 拥有 Work/Task/Knowledge/Model policy 的业务编排；`kynxa-agent-runtime` 运行 DeepSeek Harness；`kynxa-authority.exe` 是独立 Rust TCB；本地模型由 `kynxa-model-runtime` 或同等受控 runtime 管理；浏览器、屏幕、文档解析、Python ML 任务使用按需 worker。

### 生命周期
PC 开机后仅 Resident/网络组件允许轻量常驻。Desktop/Host 可由用户或远程控制启动；模型与浏览器不得因为 Resident 在线而常驻。退出 GUI 时可选择“停止所有任务”或“后台继续”，该选择必须清楚反映到 Host 生命周期。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | Windows-first Local Agent Host 产品拓扑 |
| OWN-02 | WinUI Desktop、C# Host、Node/TypeScript DeepSeek Harness Runtime、Rust Authority、Model Runtime、Resident/Network sidecar 的进程边界 |
| OWN-03 | Chat / Work / Knowledge / Models / Skills / Remote / Inbox 的产品信息架构 |
| OWN-04 | 启动、停止、升级、崩溃恢复与最小驻留策略 |

### A.2 明确不拥有
- 各模块内部算法细节
- 具体 Provider API 字段
- 每个 Skill 的业务 SOP

## B. 核心对象目录

AppSession；HostInstance；RuntimeInstance；AuthorityInstance；WorkRef；ConversationRef；TaskRef；ResidentState；NetworkProfileRef。

## C. 服务职责目录

kynxa-desktop.exe；kynxa-host.exe；kynxa-agent-runtime (Node/TS)；kynxa-authority.exe；kynxa-model-runtime；kynxa-resident；kynxa-net sidecar；optional Python workers。

## D. 主流程与状态推进
1. Desktop 启动 Host，Host 完成 DB migration 与 Authority handshake
2. 普通 Chat 可创建无 Work 的 Conversation；Work 是长期 scope
3. 需要 Agent 循环时 Host 创建/恢复 DSH session 并绑定 TaskNode
4. 危险 Tool Intent 经 Authority 生成 Grant 后由 Restricted Executor 执行
5. 长任务结果、Artifact、CognitiveState 与 Session lineage 都可在 Host 重启后恢复

## E. 不变量与安全要求
- Desktop 不直写 authority.db
- Agent Runtime 不拥有数据库安全真相
- Python 仅作为 specialized worker，不再作为中央 Agent Orchestrator
- Work 逻辑树与磁盘目录解耦
- 本地模型不可用时不能自动把内容上传云端，除非策略明确允许

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | Agent Runtime crash→Host 重启并 resume session |
| F-02 | Model Runtime OOM→按用户策略降级量化/模型/CPU 或失败 |
| F-03 | Authority crash→受保护动作全部停止 |
| F-04 | Desktop crash→Host 可继续后台任务或进入可恢复状态 |
| F-05 | DB migration failure→只读/恢复模式，不继续写入 |

## H. 指标目录

startup_time_ms；host_ready_time_ms；runtime_restart_total；task_resume_success_rate；authority_handshake_fail_total；resident_idle_memory_mb。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 冷启动到可聊天 |
| T-002 | 运行中杀死 Agent Runtime 后恢复 Task |
| T-003 | 运行中杀死 Desktop，后台 Task 状态保持 |
| T-004 | Authority 断开时危险动作 fail closed |
| T-005 | 多个 Child Work 移动时循环检测正确 |

## J. 实施顺序
1. Vertical Slice：Work→repo→DSH agent→sandbox build→patch→test→checkpoint→kill/resume
2. 再接 Knowledge/Model Management/Remote
3. 最后启用 routine/commerce 等高风险域

## K. 可启动的最小进程组合

### K.1 第一阶段责任与启动顺序〔建议〕

首个实现只需 Desktop、.NET Host、独立 Rust Authority、DSH Runtime 和受限 Code Executor。模型可先使用一种经治理的 Provider；本地模型管理、Resident、Android 和完整知识库不要求同时完成。Fast Chat 是协议和流式 UI 的中间验收，CodeRepair 才是 Agent 闭环验收。

Desktop 连接 Host；Host 检查单实例锁、数据库版本与恢复游标，再连接 Authority。Authority 先完成安全库完整性检查、进程身份验证和协议协商，随后 Host 才公布可执行能力。DSH Runtime 按需启动，未准备好时不伪造 Ready。Authority 不可用时可保留历史查看及被证明安全的纯展示功能，受保护动作停止。

建议健康状态为 STARTING、READY、DEGRADED、RECOVERING、STOPPING、STOPPED。READY 必须来自实际握手与依赖检查；进程存在或端口打开都不够。心跳只说明可达，不证明 Task 已提交，也不作为授权事实。

### K.2 启停与升级

同一用户和同一数据根目录只能有一个 Host 业务写者。实例启动时产生 instance_id/epoch，旧实例消息不能驱动新实例执行。Desktop 关闭时明确选择后台继续或停止；后台继续不依赖窗口对象存活。停止流程依次阻止新调度、请求取消、核实在途副作用、提交 checkpoint，再退出进程。

升级前备份业务库与版本清单，核查 DSH Session 格式兼容。存在 UNKNOWN 副作用时，不通过换版本或回滚数据库来抹除其状态。回滚程序版本前确认数据库格式兼容；不兼容时进入只读恢复，不对新库执行旧迁移脚本。

### K.3 当前仓库与迁移语言

截至本轮仓库检查，实际代码位于 apps/desktop，尚无旧 Python Orchestrator 或自研 Agent loop 可迁移。附录 A 中“替换、双跑、删除旧 loop”是针对历史架构的迁移条件；本仓库应先新建适配层，再以独立基准实现做对照，不创建一套旧 loop 只为完成迁移步骤。

README 中的 Rust Host 与 R3 的 .NET Host 不一致；后续实现以 R3 冻结结论为准。桌面项目的 target framework、NuGet 版本和包身份是当前工程配置，不代表设计中所有运行时已具备对应兼容性。


# 02 · 产品功能需求与 GUI 交互详细设计

## 文档目的
R3 不改变已经冻结的视觉方向：WinUI 3、简洁、留白、信息密度适中。重点是让 DSH 内核“不可见地”融入产品：用户看到的是 Chat/Work/任务/审批/模型，而不是内部 agent-loop 概念。GUI 必须能够解释当前工作状态、模型、工具、权限和恢复，而不暴露 chain-of-thought。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| GUI-001 | 基准画布 | 1440×900 作为标准设计基准，布局使用响应式 Grid/SplitView。 |
| GUI-002 | 区域可拖动 | 侧边栏、预览区、工作区分栏支持拖动调整；尺寸写入本地 UI state。 |
| GUI-003 | 顶部栏 | 保留收起侧边栏、搜索、上下文/模型入口，可后续增加 Preview 等现有体系内组件。 |
| GUI-004 | 模型管理 | “模型配置”统一改名“模型管理”，独立小型浮动窗口/子窗口，支持拖动、关闭、最小化、最大化。 |
| GUI-005 | Work | Work 内对话继续出现在左侧栏目；输入框/消息样式与普通 Chat 保持一致。 |

## 1. 初始界面与导航
左侧导航保持窄而清晰：新建 Chat、搜索、最近对话、Work、Library、Knowledge、Models、Skills、Remote/Tasks、Settings。最近对话数量应足够形成自然滚动，不使用过早渐隐来假装内容结束。图标采用统一 SVG 资产并控制在小尺寸，避免导航抢占主体。顶部栏是独立可调整区域，收起侧栏和搜索位置固定语义。

## 2. Chat
消息区支持 streaming、引用、附件、代码块、工具状态、任务状态。对普通用户只显示“正在读取文件/正在运行测试/等待批准”等可理解步骤；Developer Mode 才显示 DSH session_id、tool_call_id、model_call、profile、checkpoint 等调试信息。消息输入区保持统一视觉，不因进入 Work 更换一套交互。

## 3. Work
Work 是长期容器而非文件夹。左侧在 Work Scope 下显示该 Work 的 Conversation 列表；主体仍是标准对话框。Work Overview 展示目标、最近活动、Tasks、Knowledge、Artifacts、待审批项与模型策略。Child Work 以树形层级展示，但内容访问仍按独立 Scope。

## 4. 模型管理窗口
三个主选项：本地模型 / 官方模型 / 自定义模型；保留“全部 / 个人 / 共享”三段式平滑切换。自定义模型至少包含连接名称、协议、Base URL、API Key（Credential Broker 引用）、Model ID、模型列表获取、认证方式、Headers Secret、Network Profile、连接/首 Token/总超时、Retry、能力检测、Context Length、Endpoint Overrides、TLS/自定义 CA、成本与生成参数。

## 5. Preview 与 Artifact
附件/PDF/DOCX/PPTX/图片/代码可在右侧 Preview Pane 打开，Pane 宽度可拖动。引用点击定位到页码、段落、代码符号或网页来源。Artifact 支持版本、diff、导出与“加入 Library/Knowledge”区分操作。

## 6. 审批与失败
Approval Card 必须说明动作、资源、风险、影响范围、是否一次性、可否撤销。拒绝后 Agent 得到结构化 Observation 继续调整，不把拒绝当异常崩溃。失败卡显示“模型失败/工具失败/Authority 拒绝/网络失败/恢复中”不同类别。

## Definition of Done
- 1440×900 无遮挡
- 所有关键区域支持键盘与鼠标操作
- Work 与 Chat 视觉一致但 Scope 清晰
- 模型管理不泄露 Secret
- Developer Mode 与普通模式信息层级分离

### 1440×900 基准布局
以 1440×900 为设计基准，左侧导航栏、顶部栏、主对话区、可选右侧预览区均支持 SplitView/GridSplitter 类交互。所有尺寸使用 logical pixel，并对 125%/150% DPI 做缩放测试。左栏收起后保留图标 rail；最近对话区可滚动，不使用过早渐隐遮住项目。

### 模型管理
“模型配置”统一更名“模型管理”。窗口包含“本地模型 / 官方模型 / 自定义模型”，并保留“全部 / 个人 / 共享”平滑分段切换器。自定义连接支持 OpenAI Compatible、Anthropic Compatible、Custom Adapter、Base URL、API Key(Credential Broker)、Model ID 获取/手动添加、Headers/Secret、Network Profile、连接/首 Token/总超时、Retry、能力检测、Context Length、Endpoint Overrides、TLS/自定义 CA、成本与生成参数等。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | 1440×900 基准 WinUI 3 桌面信息架构 |
| OWN-02 | 侧边栏、顶部栏、主内容区、预览区与可拖动分隔条 |
| OWN-03 | Chat/Work/Models/Skills/Knowledge/Inbox/Settings 页面 |
| OWN-04 | 流式消息、附件、引用、预览、审批卡、任务状态与错误恢复交互 |
| OWN-05 | 模型管理独立可拖动小窗口 |

### A.2 明确不拥有
- 模型推理实现
- Authority 内部策略求值
- 网络隧道实现

## B. 核心对象目录

NavigationItem；ConversationCard；WorkCard；MessageBlock；AttachmentRef；CitationRef；PreviewPaneState；ApprovalCard；ModelManagerWindowState；SplitterLayout。

## C. 服务职责目录

ShellWindow；NavigationViewModel；ChatViewModel；WorkViewModel；PreviewService；ApprovalCenterViewModel；ModelManagerWindow；LayoutPersistenceService。

## D. 主流程与状态推进
1. 启动默认进入 Chat；侧边栏支持折叠和宽度拖动
2. 消息流式到达时做 chunk coalescing，避免逐 token UI 更新
3. 点击引用/附件在预览区打开，预览区宽度可拖动
4. 进入 Work 后左侧仍保留该 Work 的 Conversation 列表
5. 模型管理从 Settings/顶部入口打开独立子窗口，支持关闭/最小化/最大化/拖动

## E. 不变量与安全要求
- UI 不显示安全能力即等于授权；审批必须来自 Authority Intent
- 普通用户不直接编辑数据库/路径/secret
- 所有可拖动区域保存合法 min/max 尺寸，窗口缩小时先保证主对话可用
- 对话图标、导航图标保持统一小尺寸与 WinUI 视觉层级
- 不以 WebView2 作为主 GUI 实现

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | Preview renderer 不支持→回退到系统打开/文本摘要 |
| F-02 | 流式断线→保留已显示内容并提供 Retry/Resume |
| F-03 | 模型不可用→显示 capability/fallback 原因 |
| F-04 | 审批过期→卡片明确标记 Expired 而不是静默失败 |
| F-05 | 布局配置损坏→回到 1440×900 默认布局 |

## H. 指标目录

first_interactive_ms；stream_ui_updates_per_sec；preview_open_ms；approval_decision_latency_ms；layout_restore_fail_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 1440×900、1920×1080、125%/150% DPI 无裁切 |
| T-002 | 侧边栏/预览/顶部区域拖动后重启恢复 |
| T-003 | Work 内多 Conversation 左栏可滚动并保留最近对话渐进感 |
| T-004 | 模型管理窗口独立拖动/最大化且不阻塞主窗口 |
| T-005 | 大量 token 流式时 UI 线程无明显卡顿 |

## J. 实施顺序
1. 先冻结 Shell/Chat/Work/Preview 基础组件
2. 再接 Approval/Model Manager
3. 最后接 Android Remote 状态与开发者诊断视图

## K. 界面状态与后台契约衔接〔建议〕

### K.1 原型转真实功能

示例 Work、模型卡片、设备在线灯和连接诊断必须显式标识演示状态，或由真实数据替换。连接测试应展示 NOT_TESTED、TESTING、PASSED、FAILED、CANCELLED，并提供检测时间、endpoint 指纹和测试范围。上次测试成功不等于当前可达，失败也不能覆盖已经保存且有效的连接。

保存连接与测试连接是两个独立命令。测试只使用当前编辑快照；用户修改 endpoint、认证或模型后，旧结果标记过期。API Key 输入提交到受信凭证入口后清空，普通 ViewModel 只持 CredentialRef 和脱敏摘要。安全库不可用时不退回明文配置。

### K.2 消息与 Work 投影

每次发送生成 client_operation_id，按钮重复点击和重连发送同一 ID 不创建第二条用户消息。消息至少区分 queued、streaming、completed、interrupted、failed、cancel_requested。停止生成后保留已显示内容和终止原因；仅收到了部分 token 不能显示“任务已完成”。

Work 列表和对话列表由 Host 分页返回，选中项用稳定 ID 保持。切换 Work 后取消旧订阅并核对响应 scope，迟到结果不得渲染到新 Work。Work 内对话不再固定为两个示例按钮，应使用可滚动列表；空态、归档态、无权限态分别处理。

### K.3 布局、可访问性与验收

布局状态只保存视觉偏好，不保存业务恢复事实。恢复时校验有限数值、版本、min/max 和当前屏幕边界；未知新版本回退默认，不猜测迁移。缩放拖动既要支持鼠标，也要提供键盘增减和重置；PointerCaptureLost 应终止拖动，Esc 恢复本次拖动前尺寸。

建议覆盖 1440×900 和 1920×1080、125%/150% DPI、缩小窗口、多显示器移动、长中文标题以及键盘导航。logical px 与窗口 API 的物理像素需要显式转换。预览栏不能只预留宽度字段：开启、关闭、拖动、焦点返回和小窗口回退都需验收。


# 03 · Work、Conversation 与模型无关 CognitiveState

## 文档目的
R3 坚持“长期状态不能依赖某个模型或某个 DSH Session”。DSH Session 是一次/一段 Agent 执行历史；KYNXA Work/Task/CognitiveState 才是跨模型、跨进程、跨设备和跨版本可恢复的业务状态。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| STATE-001 | Work Tree | 每个 Work 最多一个 parent_work_id，禁止循环。 |
| STATE-002 | Conversation | work_id 可空；NULL 表示普通 Chat。 |
| STATE-003 | Task | Task 属于 Work，可选绑定 Conversation。 |
| STATE-004 | SessionBinding | 一个 TaskNode 可绑定当前 DSH session_id，并保留历史 generation/session lineage。 |
| STATE-005 | CognitiveState | 保存继续任务的最小充分状态，不保存模型私有 KV cache 作为真相。 |

## 1. Work Scope
Work 聚合 Conversation、Task、Knowledge、Artifact、Decision、Memory、ModelPolicy 与 PermissionPolicy。磁盘不跟随逻辑树嵌套移动，避免重命名/移动 Work 引发大规模文件搬迁。父 Work 可聚合元数据，但不得自动继承子 Work 私有知识或 Grant。

## 2. CognitiveState Schema
建议字段：goal、constraints、task_graph_ref、current_node、completed_node_refs、unresolved_questions、decision_refs、evidence_refs、observation_refs、artifact_refs、environment_snapshot、model_policy_revision、permission_scope_ref、resource_budget、verifier_state、recovery_cursor、revision。所有大 payload 使用内容寻址引用，避免单行 JSON 无限制增长。

## 3. TaskGraph 与 DSH 的两层关系
外层 Durable TaskGraph 由 Host 管理，负责依赖、生命周期、预算、审批、跨重启恢复。某个 MODEL/AGENT 类型 TaskNode 的内部实现可以是一段 DSH Session agent loop。DSH 可在该 Session 内自主调用工具、subagent 或 workflow，但它不能越过 TaskNode 的 Scope/预算/Capability envelope。

## 4. SessionBinding
SessionBinding 至少包含 binding_id、task_id、node_id、dsh_session_id、profile、cwd/work_mount、context_revision、model_policy_revision、created_at、released_at、resume_strategy。Agent Runtime 重启后先验证 Session header/cwd 与 Work Scope，再恢复；不允许拿另一个目录的 Session 静默继续。

## 5. Reducer 与事件
CognitiveState 的变更由可审计事件驱动，如 NodeStarted、ObservationCommitted、DecisionAccepted、CheckpointCreated、ApprovalResolved、ArtifactProduced、SessionRebound。模型自然语言不能直接写入 final state；必须先转换为结构化事件并由 Host reducer 校验。

## Definition of Done
- 换模型后可继续任务
- Agent Runtime/Host kill-resume 通过
- Child Work scope 不串
- Session 与 Task 映射可追踪
- 状态 revision 并发冲突可检测

### CognitiveState 最小建议字段
```text
CognitiveState {
  task_id, graph_revision, policy_revision,
  goal, constraints, current_step,
  completed_steps[], unresolved_questions[],
  decisions[], observations[], evidence[],
  retrieved_knowledge[], retrieved_experience[],
  active_skills[], verifier_state,
  artifacts[], environment_state,
  permission_state, resource_budget,
  session_lineage[], updated_at
}
```
不得把模型隐藏推理、KV Cache 或某个 Provider 的私有线程 ID 作为唯一恢复依据。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | Work/Child Work 长期 Scope |
| OWN-02 | Conversation、Task、TaskNode 与 CognitiveState 分层 |
| OWN-03 | Checkpoint/Snapshot/Handoff/Context Compiler |
| OWN-04 | TaskGraph revision 与跨模型恢复 |
| OWN-05 | DSH SessionBinding 与 session lineage |

### A.2 明确不拥有
- DSH 内部 turn/step 历史实现
- 长期 Knowledge 本体
- Authority Grant 真相

## B. 核心对象目录

Work；Conversation；Task；TaskNode；CognitiveState；Checkpoint；SessionBinding；ContextBundle；ArtifactRef；DecisionRef。

## C. 服务职责目录

WorkService；TaskService；CognitiveStateReducer；CheckpointService；ContextCompiler；SessionBindingService。

## D. 主流程与状态推进
1. Conversation 可属于 Work 或为普通 Chat
2. TaskGraph 外层负责 durable node、依赖、审批、预算、恢复
3. RUNNING_AGENT 节点创建/恢复 DSH SessionBinding
4. 模型切换通过 ContextBundle/Handoff，而不是迁移 KV/CoT
5. Checkpoint 固化关键 Observation、Artifact、side-effect 引用与 graph_revision

## E. 不变量与安全要求
- CognitiveState 不保存私有 chain-of-thought 作为业务真相
- Work 删除采用 tombstone/retention，不直接抹除审计
- Child Work 具有独立 scope，不因父级查看而自动扩权
- session_id 不是 task_id；一项 Task 可产生 session lineage
- Context 编译必须显式带 scope/revision 和 provenance

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | session 丢失→从最新 Checkpoint 创建新 generation |
| F-02 | graph revision 冲突→节点停止并重新规划 |
| F-03 | 并发修改 Work→optimistic revision conflict |
| F-04 | Context 超预算→按重要性压缩，Authority/side-effect refs 不得摘要丢失 |
| F-05 | Artifact 缺失→标记 degraded evidence |

## H. 指标目录

checkpoint_write_ms；resume_success_rate；context_bundle_tokens；graph_revision_conflict_total；session_generation_count。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 模型 A→模型 B 后目标/约束/证据/Verifier 状态不丢 |
| T-002 | Host kill 后从 checkpoint 恢复 |
| T-003 | 移动 Child Work 做循环检测 |
| T-004 | 同 TaskNode 两个并发 worker 不允许重复 side effect |
| T-005 | 长期会话 compaction 后仍能恢复关键决策 |

## J. 实施顺序
1. 先实现 Work/Conversation/Task schema
2. 再实现 reducer/checkpoint
3. 最后接 DSH session mapping 与跨模型 handoff

## K. Durable TaskNode 与恢复状态机〔建议〕

### K.1 状态、执行尝试与取消

TaskNode 保存 status、revision、attempt_no、active_execution_id、lease_owner、lease_expires_at、fencing_token、checkpoint_ref。重试产生新的 execution_id，不覆盖旧尝试。Node 状态描述业务推进；Session 状态和 SideEffect 状态分别存储，不用同一枚举代替。

| 当前状态 | 触发与前置条件 | 下一状态 |
|---|---|---|
| PENDING | 依赖成功且预算可用 | READY |
| READY | 条件更新成功并获得执行 lease | RUNNING |
| RUNNING | 需要 Authority 用户批准 | WAITING_APPROVAL |
| WAITING_APPROVAL | 同一请求获有效 Grant，执行 lease 仍有效 | READY |
| RUNNING | 产物已提交，等待确定性验证 | VERIFYING |
| VERIFYING | 所有必需验证通过 | SUCCEEDED |
| RUNNING / VERIFYING | 进程失联或提交结果不明 | RECOVERING |
| RECOVERING | 确认无在途副作用，恢复锚点有效 | READY |
| RECOVERING | 副作用未决或依赖无法满足 | BLOCKED |
| 非终态 | 用户取消 | CANCEL_REQUESTED |
| CANCEL_REQUESTED | 执行器停止且在途动作已核实 | CANCELLED |
| 非终态 | 不可恢复且无未决副作用 | FAILED |

WAITING_APPROVAL→READY 表示重新进入调度检查，不能重新生成并重复提交同一 ToolIntent。拒绝批准后 Host 把拒绝 Observation 交回 Session；Agent 可以提出不同方案，但新方案必须有新的请求身份和审批摘要。审批未解决时禁止后台继续该受保护动作。

PAUSED 作为调度控制字段可与 BLOCKED 区分：用户暂停不等于失败。任务存在 UNKNOWN 副作用时，取消请求可以阻止未来调度，但不能假报所有在途动作已取消；UI 显示“已停止新增操作，正在核实已发出操作”。终态不可原地重开，用户重试创建新的 execution 或新任务并保留 lineage。

### K.2 Lease 与 fencing

Host 在短事务中以 expected_revision 和状态条件争抢 Node，成功后递增 fencing_token。所有 ToolIntent 和执行回执绑定 execution_id 与该 token。Executor 在接收动作时检查 token；旧 worker 即使恢复连接也不能继续提交新副作用。单靠 lease 到期不足以证明旧进程停止，需撤销能力并确认旧执行器已终止或被隔离。

RUNNING lease 到期只进入 RECOVERING，不立即并行启动新尝试。先查询 Authority ledger 和 Executor receipt，再判断能否重新执行。跨外部服务仍不能保证 exactly-once：fencing 只阻止系统控制边界内的新旧执行争用，无法撤回已经到达外部服务的请求。

### K.3 Checkpoint 一致性

Checkpoint 包含 task_id、graph_revision、cognitive_revision、execution_id、binding generation、已消费事件游标、artifact/version/hash 和 side_effect_ref。它必须引用已提交的事实；不可把尚在内存中的 ToolResult 当成已完成。Host 在同一业务事务提交 reducer 输出与 checkpoint 元数据，大 payload 先完成 blob 持久化。

恢复时依次核对 Work scope、图版本、Session 绑定、执行 lease、Authority 副作用状态和 Artifact hash。若 Session 丢失但 checkpoint 可用，可建立新 generation；仍保留旧 Session 引用及恢复原因。缺失关键 Evidence 时降级为待验证，不能以模型总结替代原始结果。

### K.4 Work 树与上下文预算

移动 Work 在事务内校验目标父级不等于自身，且目标祖先链不包含当前 Work；并发移动需由同一树写者串行处理。逻辑父子关系不授予内容读取或 Grant 继承。归档限制新任务；删除使用 tombstone，关联审计按独立保留政策保存。

ContextBundle 至少划分 pinned constraints、current task、verified observations、retrieved evidence、preferences 和 recent dialogue。预算不足时先删除低相关、可重新检索的材料；Authority 约束和在途副作用引用不得被一般摘要抹掉。任何被引用的私有内容都需再次检查当次 scope 和网络外发政策。


# 04 · 知识、记忆、经验与决策记忆系统

## 文档目的
R3 保留四类长期信息边界，不因 DSH 有 Session history/compaction 就把长期知识塞回聊天历史。DSH 只消费 KYNXA Context Compiler 输出的受 Scope 约束 Evidence/Memory/Decision 摘要与引用。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| KN-001 | Knowledge | 外部事实与来源，可版本化、可引用。 |
| KN-002 | Memory | 对用户/Work 持续重要的偏好与状态。 |
| KN-003 | Decision | 已做出的架构/业务决定，带 rationale/evidence/supersedes。 |
| KN-004 | Experience | 环境+策略+结果的可复用经验，不自动升级成权限。 |
| KN-005 | Skill | “如何做”的流程，执行交由 DSH skill/tool，但治理由 KYNXA。 |

## 1. Scope 与检索
默认优先级：当前 Conversation 附件→Work Knowledge→Personal Global→Community→Web（若允许）。普通 Chat 不访问任何 Work 私有源。Dense、sparse/FTS、metadata filter、rerank 共同构成 Hybrid RAG；引用必须保留 source_id/version/page/path/symbol。

## 2. Context Compiler
R3 的 Context Compiler 不再直接拼一个“给旧 Python Planner 的巨大 Prompt”，而是输出结构化 ContextBundle：Work Summary、Current Task Node、Constraints、Knowledge Evidence、Decision Refs、Relevant Memory、Artifact Refs、Capability Summary、Authority Constraints。DSH system-prompt/context extension 将其装配到 agent request。

## 3. DSH Session 压缩边界
DSH compaction 用于缩短当前 Session 历史；KYNXA 不把 compaction 摘要自动当作长期 Memory。需要持久晋升的信息必须通过 Memory/Decision/Experience proposal，带 provenance 与用户/规则确认。

## 4. Experience 晋升
Trace 中出现稳定成功策略时可生成 Experience Candidate；跨任务证据、回放、Verifier、错误边界稳定后才可推荐生成 Skill Candidate。单次成功不能直接覆盖全局行为。

## 5. 版本与冲突
同一 Knowledge Source 更新后旧 chunk 不立刻物理删除；通过 source_version/current 标记和 GC 延迟回收。Decision 使用 supersedes 链解决“旧决定看起来仍有效”的问题。检索发现重要冲突时必须显式返回冲突证据，而不是让模型自行选择无说明版本。

## Definition of Done
- 跨 Work 检索隔离
- 引用可跳转
- 删除索引可重建
- Session 摘要不会污染长期记忆
- 冲突/版本有测试

### 检索优先级
Work 内默认：当前 Conversation 附件 → Work Knowledge → Personal Global → Community → Web（若允许）。普通 Chat 不默认读取任意 Work 私有源。Dense/Sparse 只是召回手段，最终排序还要加入 scope、source quality、freshness、version compatibility 与 conflict signal。

### 文档预览与引用
PDF/DOCX/PPTX/图片/代码进入 Knowledge 后仍保留原始 Artifact/FileRef，回答中的 CitationRef 必须能够回到具体 source/version/page/section/line 范围，而不是只给向量 chunk ID。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | Conversation/Work/Personal/Community 四层 Knowledge |
| OWN-02 | Memory、Decision Memory、Experience 与 Skill 的职责分离 |
| OWN-03 | 解析、Chunk、Hybrid Retrieval、Rerank、Provenance、时间版本 |
| OWN-04 | Work Capsule 与可忘记/GC |
| OWN-05 | RAG Evidence 到 ContextBundle 的可追溯映射 |

### A.2 明确不拥有
- Tool 权限
- 模型内部记忆
- Session event log

## B. 核心对象目录

KnowledgeSource；DocumentVersion；Chunk；EmbeddingRecord；MemoryItem；DecisionRecord；ExperienceRecord；ProvenanceEdge；RetrievalTrace。

## C. 服务职责目录

IngestionService；ParserWorkers；Chunker；FTSIndex；VectorIndex；Reranker；MemoryGovernance；DecisionMemoryService；ExperienceBank。

## D. 主流程与状态推进
1. Source 导入→hash/version→解析→chunk→FTS/vector 索引
2. 查询先按 scope/ACL/时间过滤，再 dense+sparse 召回并 rerank
3. 关键 Decision 以 supersedes 关系更新，不靠文本覆盖
4. Experience 由任务结果与 verifier 证据晋升，失败经验同样可记录
5. Evidence 以引用形式进入 ContextBundle，保留 source/version/chunk offsets

## E. 不变量与安全要求
- Vector DB 不是 Source of Truth
- Knowledge 不等于 Memory，事实不能因“记忆”而覆盖更新文档
- Community/Web 内容始终是不可信输入
- 删除/忘记必须能传播到索引与 derived cache
- 跨 Work 默认禁止检索私有知识

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | parser crash→隔离重试并保留原文件 |
| F-02 | embedding provider 变更→异步重建索引 |
| F-03 | source 文件更新→生成新 version，不就地篡改 provenance |
| F-04 | 检索冲突→显式展示版本/来源而非强行合并 |
| F-05 | 索引损坏→从 SQLite+files 重建 |

## H. 指标目录

ingest_docs_per_min；retrieval_recall_at_k；rerank_latency_ms；stale_index_ratio；orphan_chunk_total；memory_gc_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | Work A 查询不能命中 Work B 私有文档 |
| T-002 | 更新文档后旧引用仍可解析到历史版本 |
| T-003 | 删除源后索引最终一致清除 |
| T-004 | Decision supersedes 链可追溯 |
| T-005 | RAG answer citation 可跳转到原文页/段 |

## J. 实施顺序
1. 先做文件/PDF/DOCX/代码 ingestion + FTS
2. 再加 vector/rerank
3. 最后加 Experience/Decision/Community pack

## K. 知识入库与可撤销检索〔建议〕

Source 先保存来源与访问范围，再计算内容版本，解析工作在受限 Worker 运行。入库状态建议为 RECEIVED→STORED→PARSED→INDEXING→READY；解析失败保留原文件及错误引用，索引失败只影响检索，不删除原文件。用户能区分“文件已保存”和“已经可检索”。

Chunk 身份由 document_version_id、解析器版本和定位信息确定；embedding_model_id/dimension/index_generation 作为索引元数据，换模型不覆盖旧向量空间。旧引用保留到历史版本；物理文件只去重字节，不把不同 scope 的授权记录合并。

检索先做可访问集合过滤，再进行召回；返回前再次验证权限 revision，避免索引延迟导致越权。删除源时立即 tombstone 并从查询结果中过滤，随后异步清理向量、FTS 和缓存。即使索引删除失败，也不能继续把已删除源交给模型。

Memory/Decision/Experience proposal 需记录提出者、证据、scope、置信度和状态 CANDIDATE/ACCEPTED/REJECTED/SUPERSEDED。单次 Session 压缩结果只作为候选材料。冲突决策通过 supersedes 关联；没有足够证据时呈现冲突，不自动用最新一段模型文本覆盖事实。


# 05 · Skill、Tool、MCP 与社区能力生态

## 文档目的
R3 最大变化之一是“执行复用 DSH，治理保留 KYNXA”。DSH 已提供 tool registry、skill discovery/loading、MCP resources、subagent/workflow 等能力，KYNXA 不再实现一套重复执行内核；KYNXA 负责来源、版本、锁定、Scope、Capability 映射、供应链、回归和 UI。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| SK-001 | Skill Runtime | 优先复用 DSH skill/skill-filesystem/tool-skill。 |
| SK-002 | Tool Surface | Tool 由 DSH 暴露给模型，但高风险 Tool 统一包装为 KYNXA Authority Tool Adapter。 |
| SK-003 | MCP | MCP Tool schema 先 normalize，再映射 KYNXA Capability；MCP server 不是可信边界。 |
| SK-004 | Governance | Install ≠ Trusted ≠ Authorized。 |
| SK-005 | Fork policy | 优先 plugin/profile/patch；只有无 extension seam 时才 fork DSH 核心。 |

## 1. 三层模型
Capability 是 Authority 能表达的原子权限；Tool 是 DSH/Host 暴露的结构化动作；Skill 是可复用 instruction/procedure。Skill 安装不能自动获得 Tool 权限，Tool 注册不能自动获得宿主权限。Experience 只是证据。

## 2. Skill 包与 DSH 映射
KYNXA 继续支持 SKILL.md + 可选 kynxa.skill.yaml/resources/scripts/tests/kynxa.lock。安装时把可供模型读取的 instruction material 投影到 DSH skill provider；manifest 中的 capability、source hash、trust、version 由 KYNXA Registry 保存。scripts 不直接由 DSH 任意执行，必须通过 Tool/Authority 路径。

## 3. Tool Adapter
低风险纯计算 Tool 可直接在 sandbox/worker 内执行。filesystem.write、process.host、browser.submit、credential.use、computer.input 等统一注册成包装 Tool：模型调用→validate schema→CapabilityRequest→Authority→受限执行→Observation。Tool result 必须带 execution_id 与 side_effect_ref。

## 4. MCP
远端/本地 MCP 的 discover/call 只解决“服务暴露了什么”；KYNXA 必须重新计算网络域、Credential、资源 Scope 与风险。MCP 的 input_required/confirmation 不等价于 KYNXA Approval。Schema hash 变化使缓存/SkillIR 失效并触发重新验证。

## 5. Subagent / Workflow
多 Agent 与 orchestration 直接复用 DSH subagent/workflow。Host 通过 budget envelope 限制并发、token、工具和时间；child session 的权限不因 parent 强而无限继承，仍受 TaskNode/Work Scope。外部 Codex/Claude/其他 Harness backend 只作为 less-trusted provider。

## 6. 供应链
Community Skill 固定 source commit/hash/license；禁止默认追踪 main。更新新增 Capability 时重新授权。无许可证或含危险安装脚本的包只允许隔离实验，不进入 Verified。所有 Skill promotion 需要 smoke/regression/safety/replay。

## Definition of Done
- DSH Skill 可加载且 KYNXA Registry 可追踪
- 危险脚本无法绕过 Authority
- MCP schema 变化可检测
- subagent 不跨 Scope
- 升级 DSH 后 Skill 回归通过

### Skill 包
```text
my-skill/
├── SKILL.md
├── kynxa.skill.yaml        # 可选：capability/model/runtime 声明
├── kynxa.lock              # 固定依赖/commit/hash
├── references/
├── templates/
├── examples/
├── scripts/
└── tests/
```
纯 instruction skill 只需 SKILL.md。脚本永远通过受控 tool adapter 运行，不因文件位于 skill 目录而获得执行权。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | Tool 原子执行语义与 Skill Procedure/SOP 分离 |
| OWN-02 | SKILL.md-first 包格式、manifest/lock/resources/scripts/tests |
| OWN-03 | MCP 与第三方仓库适配 |
| OWN-04 | Skill registry、版本、信任、回归与来源锁定 |
| OWN-05 | DSH skill/tool/MCP runtime 复用和 KYNXA governance |

### A.2 明确不拥有
- 权限授予
- 模型选择最终决定
- 原生 Tool 的 OS 特权执行

## B. 核心对象目录

ToolDefinition；SkillPackage；SkillManifest；SkillLock；SkillVersion；ToolBinding；McpServerProfile；SkillTestResult；PublisherTrust。

## C. 服务职责目录

SkillCatalog；SkillInstaller；SkillScanner；SkillCompiler/Adapter；MCPManager；ToolSchemaBridge；RegressionGate。

## D. 主流程与状态推进
1. 安装来源固定 commit/hash→静态扫描→sandbox test→Candidate
2. Verified Skill 运行时通过 DSH 按需加载
3. Skill 声明 capability requirement，但 Authority 独立授权
4. MCP Server 暴露工具先转 KYNXA Tool schema 再进入 Tool Bridge
5. 版本升级保留 lock 与 regression result，失败可回滚

## E. 不变量与安全要求
- Install != Trusted != Authorized
- 存在 script 不代表可直接执行；必须走 Tool/Authority
- Skill preference 不能覆盖用户显式 Model/Privacy Policy
- 第三方 MCP 的 tool description 视为不可信 metadata
- Community 包不得写 policy/credential store

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | 依赖不可解析→安装失败但不污染已验证版本 |
| F-02 | MCP server 超时→断路器/熔断 |
| F-03 | Tool schema 变更→compatibility gate |
| F-04 | Skill 回归失败→保持旧 verified 版本 |
| F-05 | 供应链 hash 不一致→立即 quarantine |

## H. 指标目录

skill_install_success_rate；skill_regression_pass_rate；mcp_tool_latency_ms；tool_schema_mismatch_total；quarantined_package_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 恶意 Skill 请求读取 Credential→Authority 拒绝 |
| T-002 | Skill 升级后旧 lock 可回滚 |
| T-003 | MCP 返回 prompt injection 文本只作为 data |
| T-004 | DSH skill 按需加载不扩大 tool catalog |
| T-005 | 离线安装本地 package 可完成验证 |

## J. 实施顺序
1. 先实现本地 SKILL.md + Tool registry
2. 再接 MCP
3. 再做 GitHub/GitLab/Gitee source + trust UI

## K. 能力包安装与运行闭环〔建议〕

Skill 生命周期建议为 DISCOVERED→QUARANTINED→VALIDATED→ENABLED；更新先安装到新版本目录，验证完成后切换活动版本引用。失败不污染旧版本。source commit、内容 hash、依赖 lock、许可证和声明能力纳入安装记录；增加能力后进入重新评估，而不是沿用旧信任标签。

Tool 注册保存 schema_hash、adapter_version、capability 映射和 observation 限制。execute 回调仅能进入受控桥接，禁止模型通过未登记的 alias 调到原始宿主 shell 工具。对所有注册工具做“执行入口到 Authority/隔离 Worker”的覆盖检查；只检查工具名称黑名单不够。

MCP 超时后的重试取决于工具效果类型，不能把普通 RPC 超时统一标为 Transient。第三方声明的 readOnly/idempotent 只作候选元数据，须由本地治理映射确认。Resource 文本和工具说明保留不可信来源标记；发现描述或 schema 变化时重新计算 hash 并阻断不兼容调用。


# 06 · DeepSeek Harness 集成、持久任务编排与 Agent 执行内核

## 文档目的
本章替代旧《Adaptive Harness》作为 R3 核心重写文档。目标不是“把 DSH 嵌进去就结束”，而是明确谁拥有哪一层生命周期：KYNXA Durable Orchestration 管外层长期任务；DSH 管每个 Agent Session 内的 turn/step/tool/subagent/workflow；Rust Authority 管最终副作用。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| DSH-001 | Core spine | 使用 DSH session/system-prompt/tools/agent/agent-loop 作为可替换主干。 |
| DSH-002 | Durable session | 采用 DSH session persistence；JSONL 是 session 事实，不是 KYNXA 业务真相。 |
| DSH-003 | Composition | 建立 kynxa profile/bundle patch，只装需要能力；避免修改 upstream core。 |
| DSH-004 | Adapters | 至少实现 State/Context/Model/Tool/Event/Telemetry 六类 KYNXA↔DSH Adapter。 |
| DSH-005 | Workspace | DSH Workspace 仅作为可选 Session 项目视图，不替代 KYNXA Work。 |
| DSH-006 | Agent loop ownership | KYNXA 不再复制 ReAct loop、tool dispatch、subagent/workflow 基础设施。 |

## 1. 上游能力映射
当前 DSH 包工作区按能力家族组织：core 提供 session、system-prompt、tools、agent、agent-loop；session 提供持久会话/检查点；skill 提供按需技能；subagent 提供进程内与外部 backend 委派；workflow 支持模型编写 orchestration；web 提供搜索/抓取；storage/workspace 提供 host-side 持久应用状态。R3 采用这些 seam，而不是复制代码。

## 2. kynxa-agent-runtime
建议创建独立 Node/TypeScript 进程，基于 DSH base/minimal composition 装配 KYNXA profile。启动参数仅接受 Host 传入的 profile、socket/pipe endpoint、session home 与诊断级别。Runtime 不读取 authority.db，不持有长期 Credential；它可以缓存模型 schema/Skill catalog，但可随时重建。

## 3. Host Adapter 协议
Host→Runtime：CreateSession、ResumeSession、DeliverPrompt、InjectContext、CancelTurn、ReleaseSession、GetSessionProjection。Runtime→Host：SessionEvent、ModelCallEvent、ToolIntent、ToolResultEvent、SubagentEvent、WorkflowEvent、UsageMetric、FatalError。所有消息带 task_id/node_id/session_id/correlation_id/revision。

## 4. Context Adapter
Host 生成 ContextBundle，Runtime 插入 DSH system-prompt/context section。KYNXA 不改 DSH history 推导规则；只注入当前 Work/Task 所需的外部结构化上下文。ContextBundle 可包含摘要和引用，但不把 Secret 注入 Prompt。revision 变化时记录 context_revision，便于回放。

## 5. Model Adapter
DSH LLM seam 对接 KYNXA Model Resolver：请求方提供 capability requirements 与用户策略，KYNXA 返回具体 endpoint/model/params；本地模型通过 KYNXA Runtime 暴露兼容 provider；官方/自定义模型使用受 Network Profile 与 Credential Broker 控制的 adapter。DSH 只拿短期调用句柄或受控 token，不持久保存 master secret。

## 6. Tool Adapter 与 Authority
DSH tool registry 可展示 KYNXA Tool schema。危险 Tool 的 execute 回调不直接执行系统动作，而是向 Host/Authority 请求 Grant。ApprovalRequired 作为结构化 Tool Observation 回到 agent loop，Host UI 触发审批；批准后用同一 canonical_request_hash 继续，禁止模型偷偷换参数复用批准。

## 7. Session 与 TaskNode
每个 Durable TaskNode 至少可有 0/1 个 active session，也允许因故障产生 session lineage。Node 进入 RUNNING_AGENT 后绑定 session；Session 完成并不等于 Node 成功，Host 还可以跑 programmatic verifier。Node checkpoint 保存 binding、critical observations、artifact refs 与 side-effect state。

## 8. Subagent/Workflow 策略
DSH subagent/workflow 只在 Deep Work 或满足阈值时启用，避免普通任务过度 fan-out。Host 下发 max_children、max_parallel、token_budget、wall_clock、allowed_tool_classes。child 的 Tool 调用仍走相同 Authority Bridge。

## 9. Compaction 与长上下文
DSH compaction 负责 Session 历史可用性；KYNXA Context Compiler 负责长期知识与任务状态。compaction 不能删除/改写必须保留的 Authority/side-effect references；对长任务，Host 可新建 session generation 并注入最新 CognitiveState，而不是无限延长一个 Session。

## 10. 故障恢复
Runtime crash：Host 保持 Task RUNNING/RECOVERING，重启 Runtime，打开持久 Session，验证 SessionBinding 后 resume。模型调用中断：从已提交 Session event/Checkpoint 重新请求，不恢复半个 token。Tool 执行 UNKNOWN：先 reconciliation，再决定继续。DSH storage/query 索引损坏：重建，不影响 KYNXA state。

## 11. Upstream 同步策略
KYNXA 维护 upstream commit/semver 锁与 compatibility matrix。默认只写 plugin/profile/adapter，不修改 core。若确需 patch，保存最小补丁集、原因、测试覆盖与 upstream issue；每次升级跑 session-format、tool contract、skill、subagent、workflow、authority bridge、kill/restart 回归。

## 12. 性能策略
Fast Chat 使用精简 profile，工具/skill/subagent 可以延迟加载；Agent Work 才启用完整 Tool catalog；Deep Work 再启用 workflow/subagent。Host 对 token stream 做小窗口聚合，避免 WinUI Dispatcher 被逐 token 淹没。DSH telemetry 映射到 KYNXA Flight Recorder，但不强制记录敏感 prompt payload。

## Definition of Done
- 旧自研 agent loop 代码可删除/停用
- 同一 TaskNode 可 kill/resume DSH Session
- Authority Bridge 覆盖全部危险 Tool
- DSH Workspace 不污染 Work 语义
- 上游升级有自动回归
- Fast Chat 与 Agent Work profile 可独立

### 上游事实基线（2026-09-18）
DeepSeek Harness `packages/core` 当前把 `session / system-prompt / tools / agent / agent-loop` 定义为可替换主干；`agent-loop` 负责创建/恢复 agent、模型流式响应、tool dispatch 与 durable session history。`workspace` 是 host-side 的目录/Session 分组能力，对模型不可见；`storage` 保存非 Session 的 host-side durable application data。KYNXA 因此只在 seam 上做 Adapter，而不复制 loop。

### 两层执行模型
```text
KYNXA Durable TaskGraph
  ├─ deterministic node
  ├─ DSH Agent Session
  │    └─ turn → model → tool → observation → next step
  ├─ verifier node
  └─ authority-controlled side effect node
```
Session 完成不自动等于 TaskNode 成功；KYNXA 可以在 Session 后执行编译、测试、hash、schema 等 deterministic verifier。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | DeepSeek Harness 作为 Agent Execution Engine 的集成边界 |
| OWN-02 | KYNXA Durable TaskGraph 与 DSH Session/Agent Loop 的双层运行模型 |
| OWN-03 | State/Context/Model/Tool/Event/Telemetry Adapter |
| OWN-04 | subagent/workflow/skill/web/compaction 的启用策略 |
| OWN-05 | upstream 跟踪、bundle/profile 与最小 patch 策略 |

### A.2 明确不拥有
- 重新实现 ReAct/agent loop
- 让 DSH Workspace 替代 KYNXA Work
- 让 DSH approval/sandbox 取代 Rust Authority

## B. 核心对象目录

AgentRuntimeProfile；DshSessionRef；SessionBinding；ContextBundle；ModelRouteHandle；ToolIntent；ToolObservation；RuntimeEvent；SessionLineage。

## C. 服务职责目录

kynxa-agent-runtime；DshCompositionFactory；ContextAdapter；ModelAdapter；AuthorityToolBridge；EventAdapter；TelemetryAdapter；SessionLifecycleManager。

## D. 主流程与状态推进
1. Host 创建 TaskNode→Runtime CreateSession/ResumeSession
2. agent-loop 请求模型→LLM seam 调 KYNXA Model Resolver
3. 模型 tool call→DSH tools→KYNXA Tool Bridge→Authority/Executor
4. 结果写回 DSH append-only session log，同时投影到 Host Event Stream
5. Node 结束后 Host 再执行 programmatic verifier 并更新 CognitiveState/Checkpoint

## E. 不变量与安全要求
- 默认只通过 upstream extension/plugin/profile 扩展，不 fork core
- DSH Session 是 agent 执行事实，不是 KYNXA Work/Task 业务真相
- DSH storage/workspace 为 host-side runtime 辅助状态，不拥有 KYNXA 数据模型
- 任何 child/subagent 的危险 tool 同样经过 Authority
- Runtime 不读取 authority.db、不持有长期 credential

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | Runtime crash→重启并 resume persisted session |
| F-02 | session schema 不兼容→migration gate/新 generation |
| F-03 | tool UNKNOWN→reconcile before retry |
| F-04 | upstream 升级导致 adapter contract 变化→compat matrix 阻断发布 |
| F-05 | compaction 丢关键引用风险→Host 用 pinned context refs 校验 |

## H. 指标目录

agent_turn_latency_ms；tool_intent_total；parallel_tool_call_peak；session_resume_ms；runtime_crash_total；adapter_contract_error_total；tokens_per_task。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | kill runtime 中途恢复 session |
| T-002 | child subagent 不能绕过 Authority |
| T-003 | 用户 Strict Coder 无 vision 时返回 capability unavailable |
| T-004 | Work 与 DSH Workspace 名称/删除语义互不污染 |
| T-005 | upstream core 升级跑 session/tool/subagent/workflow 回归 |

## J. 实施顺序
1. 建立 dsh-base/minimal 的 kynxa profile
2. 实现六类 Adapter
3. 迁移旧自研 agent loop 调用点
4. 启用 subagent/workflow
5. 建立 upstream 自动兼容测试

## K. 六类 Adapter 的最小可测契约〔建议〕

本节定义 KYNXA 侧逻辑契约，不声明任何具体 DSH npm 包名、函数签名或事件格式已被验证。应先锁定真实上游 commit 和 license，通过兼容性 spike 记录持久化、恢复与工具拦截的实际能力，再实现对应 adapter。

| Adapter | 输入与输出 | 必须独立验证 |
|---|---|---|
| State | TaskNode/Checkpoint → SessionBinding | generation 不串用，失联可核实 |
| Context | ContextBundle → DSH 上下文投影 | scope、revision、预算和来源保留 |
| Model | 能力请求 → 受控 ModelRouteHandle | Strict、云策略、取消和用量 |
| Tool | ToolIntent → Authority/Worker Observation | 所有危险入口经过授权 |
| Event | 上游事件 → KYNXA RuntimeEvent | 重复、乱序、缺口、终止均可处理 |
| Telemetry | 调用与资源事件 → trace/metrics | 可关联、脱敏、不过量阻塞 |

### K.1 Session 创建幂等与孤儿处理

Host 先提交 PENDING SessionBinding，包含 operation_id、node_id、generation、profile_digest、scope_ref 和 mount_ref。Runtime 使用 binding_id 作为创建幂等键，重复 CreateSession 返回原 Session。若上游不提供原子幂等创建，Adapter 以持久 journal 实现，并把上游 Session 与 binding_id 建立可查询映射。

创建成功后 Host 再激活绑定。若 Runtime 已创建但 Host 未记录 ACK，恢复时查询 binding_id，不能盲目新建第二个活动 Session。无法确认的 Session 暂停并列为 orphan；只有 scope 与 generation 校验通过后才可重新绑定。ReleaseSession 释放运行资源，不默认删除持久历史。

### K.2 工具结果投递顺序

Runtime 产生 tool_call_id 和 operation_id；桥接层验证 schema 并建立执行记录。执行完成后以 receipt 提交 Authority ledger，Host 再提交 Observation/outbox；Event Adapter 将同一 observation_id 返回 Session。任何环节重试都返回已有结果，不能以“Session 还没看到结果”为由再次执行动作。

DSH JSONL 与两个数据库不做跨域原子事务。每个投影保存已消费游标；事件重复靠 ID 去重，序列缺口触发补拉。Runtime 只把模型和工具历史作为执行记录；Host reducer 决定 TaskNode 的成功，Verifier 可推翻未经证据支持的完成声明。

### K.3 多 Agent、预算与上游升级

默认 profile 关闭 subagent/workflow，Deep Work 满足显式策略才启用。预算 envelope 包含 max_children、max_parallel、token/cost 上限、deadline 和 allowed_tool_classes。子 Session 获得父 scope 的受限投影，不直接继承 Grant；预算由 Host 预留并在结算时释放，避免并发子任务各自使用完整预算。

上游升级前固定版本、Session fixture 和 Tool contract fixture。验证创建/恢复、工具拦截、错误观测、取消、compaction、child budget 和 kill/restart；不兼容时停止升级。记录最小补丁的原因和退出条件，不能把 upstream-only benchmark 宣称为 KYNXA 增益。


# 07 · 模型管理、本地模型运行时与异构消费级硬件自适应

## 文档目的
模型层保持 model-agnostic：用户可以用本地、官方和自定义模型；DSH 只通过统一 LLM Provider seam 使用模型。KYNXA 自己拥有模型安装、能力检测、资源准入、隐私/网络策略和故障回退。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| MODEL-001 | 模型管理 UI | 本地模型/官方模型/自定义模型三类。 |
| MODEL-002 | Conversation Mode | Smart / Prefer Selected / Strict。 |
| MODEL-003 | 能力槽 | General/Coding/Vision/Reasoning/Fast；Advanced 包含 Embedding/Reranker/OCR/Long Context/Artifact QA/Background。 |
| MODEL-004 | Local runtime | llama.cpp/GGUF 等成熟推理后端由 KYNXA Runtime 封装，不要求用户安装外部产品。 |
| MODEL-005 | DSH adapter | dsh 侧只看到 provider/model/capabilities，不决定最终硬件或隐私。 |

## 1. Model Resolver
解析顺序：硬 Privacy/Provider/Authority 限制→TaskNode required capabilities→Strict Lock→Skill user binding→Prefer Selected→capability slot→hardware/resource/availability。输出 ModelExecutionPlan，记录选择理由与 fallback chain，但不暴露隐藏推理。

## 2. 本地运行时
Model Runtime 负责模型文件、quantization metadata、runner、CUDA/CPU/Vulkan/SYCL 等 backend、KV cache、context sizing、batch、OOM recovery、热模型生命周期与 metrics。Agent Runtime 不直接持有 GPU 管理逻辑。

## 3. 自定义模型
支持 OpenAI Compatible / Anthropic Compatible / Custom Adapter。Connection Profile 保存 Base URL、Model ID、timeout、retry、TLS、Network Profile、capabilities、cost；API Key 仅保存 CredentialRef。获取模型列表与手动 Model ID 都允许。

## 4. Capability Detection
连接时先静态配置，再可选 probe：Text、Streaming、Vision、Tool Calling、Structured Output、Embedding、Audio、Reasoning、Context Length。Probe 失败不应自动认为模型不可用，可标记 Unknown 并允许用户覆盖。

## 5. Resource Scheduler
Hardware Fingerprint 记录 GPU/VRAM/RAM/CPU/driver/runtime。多个本地模型并存时根据任务优先级与预估 KV/weights 进行 admission；OOM 优先缩 context/batch/offload 或切更小本地模型，不能未经用户允许把私有 Work 发云端。

## Definition of Done
- 三类模型都可被 DSH 调用
- Strict 模式不自动切模型
- OOM fallback 不越隐私边界
- Credential 不落普通配置
- Models Used 可解释

### 能力路由
能力槽最少包含 General / Coding / Vision / Reasoning / Fast。Conversation 选择的是 Primary Model；复杂任务允许专门模型生成 Observation 回写 CognitiveState。Smart 可以自动路由；Prefer Selected 尽量保持用户模型但可在允许范围内补能力；Strict 不做隐式替换。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | 模型管理 UI：本地/官方/自定义 |
| OWN-02 | Model Registry、Provider、Model ID、能力检测与成本元数据 |
| OWN-03 | 本地 GGUF/llama.cpp-derived runtime 与硬件探测 |
| OWN-04 | 显存/内存预算、量化、KV、offload、并发与 OOM fallback |
| OWN-05 | Global→Work→Conversation→Skill→TaskNode Model Policy |

### A.2 明确不拥有
- Authority 安全授权
- DSH agent loop
- 用户长期 Knowledge

## B. 核心对象目录

ModelProvider；ModelDescriptor；ModelCapability；ModelRoutingProfile；ModelExecutionPlan；HardwareFingerprint；RuntimeProfile；ModelUsageTrace；CredentialRef。

## C. 服务职责目录

ModelManager；ProviderAdapter；CapabilityProbe；HardwareProfiler；LocalModelRuntime；ModelResolver；ResourceScheduler。

## D. 主流程与状态推进
1. 添加 provider→连接测试→获取/手动 Model ID→能力探测
2. 用户选择 Smart/Prefer Selected/Strict
3. TaskNode 提交 capability requirements→Resolver 生成 execution plan
4. 本地 runtime 根据 VRAM/RAM 决定量化/offload/context/KV
5. OOM/失败时按 policy 选择小模型/CPU/失败，不擅自 cloud fallback

## E. 不变量与安全要求
- 模型路由与 Authority 权限路由完全解耦
- API Key 使用 Credential Broker 引用
- Cloud fallback 默认 DENY
- 模型切换不改变 CognitiveState 真相
- 本地模型生命周期由 KYNXA 管理，不要求用户安装外部 Ollama

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | provider 401→credential/config error |
| F-02 | 429/5xx→受限重试与退避 |
| F-03 | 本地 OOM→资源回收/小模型/低 context/CPU fallback |
| F-04 | capability probe 不确定→标 unknown，不猜支持 |
| F-05 | 模型下载中断→断点/校验后继续 |

## H. 指标目录

ttft_ms；tokens_per_sec；vram_peak_mb；model_load_ms；oom_total；provider_error_rate；routing_fallback_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 4060/8GB 等低显存设备选择合适 profile |
| T-002 | 5090 高显存可扩大 context/batch 但不改变语义 |
| T-003 | Strict model 缺 Vision 不调用别的云模型 |
| T-004 | 自定义 OpenAI Compatible provider 获取模型列表 |
| T-005 | 本地与云切换后 task 恢复一致 |

## J. 实施顺序
1. 先做 Registry+OpenAI/Anthropic compatible
2. 再做 Local Runtime
3. 再做自动硬件 profile 与多模型协作

## K. 模型解析的确定性规则〔建议〕

### K.1 从策略到执行计划

Resolver 输入为 scope、required_capabilities、user_mode、selected_model_ref、privacy_policy_revision、network_profile_revision、资源快照和预算。先排除硬性禁止的 Provider、云外发和能力不满足者，再执行用户模式；候选排序必须稳定并有 reason_code。

Strict 只允许选定模型及其明确配置的运行参数变化；能力不足返回 CAPABILITY_UNAVAILABLE。Prefer Selected 优先选定模型，补充专门模型需满足已允许的候选集合与隐私政策。Smart 可以路由但不能越过硬约束。模式与快速/标准/深度的执行档位分开建模，不用一个 enum 同时表达两种语义。

ModelExecutionPlan 保存 plan_id、具体 model/version、provider/profile revision、实际参数、必要能力、fallback 列表、预算和失效条件。执行前再次检查策略 revision 和资源可用性。Fallback 需要重新生成计划并记录理由，不能修改既有计划后隐藏实际调用的模型。

### K.2 能力检测与资源准入

能力区分 declared、probed、unknown、unsupported，并记录检测用例和时间。Tool Calling 探测需验证参数可解析及返回值接续；HTTP 200 不代表支持完整协议。检测用合成输入，不发送用户 Work 内容；用户覆盖能力声明仍不能覆盖安全策略。

模型加载前预留 weights + KV + runtime overhead + safety margin 预算，实际数值由硬件测量确定，不能把示例 GPU 数字当保证。OOM 后释放损坏实例，按原政策降低 context/batch、offload 或换允许的模型；改变输入截断策略须显示信息损失，Strict 下换模型需用户调整选择。

取消模型流只停止后续生成，已计费 token 仍结算。重试发生在流已产生内容之后时，创建新的 generation 并明确替换/并列关系，禁止把两次回答无标记拼接。达到成本预算时停止新调用，并向用户报告已知用量与尚待对账部分。


# 08 · 能力模块：浏览器、计算机、文件、代码与 Artifact

## 文档目的
R3 把能力模块统一看作 DSH Tool Surface + KYNXA Capability/Authority Contract。模型看到结构化工具；真正执行由受限 worker 或 Authority-controlled executor 完成。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| CAP-001 | File | read 可按 Scope 低风险；write/delete 必须 Authority。 |
| CAP-002 | Code | build/test 默认 sandbox；修改宿主 repository 需 grant。 |
| CAP-003 | Browser | 读取与提交分离；submit/upload/download 按风险处理。 |
| CAP-004 | Computer | 截图/观察与 keyboard/mouse input 分离。 |
| CAP-005 | Artifact | 成果物有 provenance/version/hash，可预览与导出。 |

## 1. Tool Descriptor
每个 Tool 声明 input/output schema、capability、risk_class、idempotency、cacheability、resource_profile、network profile、timeout、observation limits。DSH registry 只消费 presentation schema；KYNXA Capability Registry 保存安全元数据。

## 2. File/Code
文件路径先 canonicalize，再判断 Work mounts/ACL。Repo 工具优先 read/search/diff；代码执行在 sandbox 或受限 build worker。Patch 可以先生成到 staging Artifact，获得批准后才应用到宿主 repository。

## 3. Browser/Web
Web search/fetch 与 interactive browser 分开。抓取内容永远是不可信 Prompt Input。browser.submit、登录、上传、购买等动作必须把最终 URL、method、关键 fields、resource scope 绑定到 Approval/Grant。

## 4. Computer Use
screen capture 只产生 Observation；鼠标/键盘 input 是更高风险 Capability。第一版 Remote 手机端不提供直接鼠标键盘控制，减少攻击面。Computer Agent 产生动作 proposal，Authority/Worker 执行并回传截图 hash/DOM state。

## 5. Artifact
Artifact 是模型/工具产生的可交付对象：文档、图片、diff、报告、代码包、数据集。记录 source task、generation、tool/model refs、hash、mime、preview、final flag。Library 是长期留存；Knowledge 是事实检索源，二者不能自动混为一体。

## Definition of Done
- 危险工具均有 CapabilityRequest
- 路径 TOCTOU 有测试
- 浏览器提交不会被网页 prompt 绕过
- Artifact 来源可追踪
- 代码执行默认隔离

### CodeRepair 首个 Vertical Slice
Work 绑定 repo → 读取工程 → 受控 build → 解析 error observation → agent 生成 patch → patch apply → tests/verifier → Artifact/diff → checkpoint → kill runtime/host → resume → 验证结果一致。该链路是 R3 的首要端到端验收，不再以“能聊天”作为 Agent Runtime 完成标准。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | File/Code/Sandbox/Browser/Computer/Web/Document/Vision/Artifact Tool family |
| OWN-02 | 输入/输出 Schema、capability class、risk level、timeout 与 verifier |
| OWN-03 | Preview/Artifact 版本与 provenance |
| OWN-04 | Computer Use 与直接 API/DOM 的优先级 |
| OWN-05 | Restricted Executor 与 sandbox 约束 |

### A.2 明确不拥有
- 最终授权
- Skill 高层流程
- 模型选择

## B. 核心对象目录

ToolRequest；ToolResult；FileRef；Artifact；BrowserSession；ComputerSession；SandboxRun；PatchSet；DocumentView；VisionObservation。

## C. 服务职责目录

FileExecutor；CodeExecutor；SandboxManager；BrowserWorker；ComputerWorker；ArtifactService；DocumentRenderer；VisionWorker。

## D. 主流程与状态推进
1. 只读工具优先并行；写操作串行且经过 canonical path
2. 代码修改先生成 patch/diff，再测试与 verifier
3. Browser 优先结构化 DOM/API，必要时再视觉/Computer Use
4. Artifact 每次重要修改生成版本与来源链
5. 文件上传/下载/外部发送均重新进入 capability check

## E. 不变量与安全要求
- 路径必须 canonicalize 并防 TOCTOU/path escape
- 工具返回内容视为 untrusted observation
- 直接屏幕/键鼠控制默认更高风险
- Artifact 不等于原始文件；必须有 source/provenance
- Replay 模式不得真实重放支付/发送等副作用

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | sandbox 超时→终止进程树并保留日志 |
| F-02 | browser page crash→重建 session |
| F-03 | 文件被外部修改→etag/hash 冲突 |
| F-04 | patch apply conflict→请求重新读取上下文 |
| F-05 | Computer worker 失去焦点→暂停而非盲点击 |

## H. 指标目录

tool_success_rate；sandbox_timeout_total；browser_navigation_ms；patch_apply_conflict_total；artifact_versions_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 路径穿越/符号链接逃逸被阻止 |
| T-002 | build→error→patch→test vertical slice |
| T-003 | Browser 页面 prompt injection 不能触发越权 tool |
| T-004 | 大文件预览不一次读入 UI |
| T-005 | Computer Use 操作前后截图/状态 verifier 可关联 |

## J. 实施顺序
1. 先 File/Code/Sandbox/Artifact
2. 再 Browser/Web
3. 最后 Computer/Vision 高风险交互

## K. CodeRepair 从输入到证据的闭环〔建议〕

### K.1 仓库读取、隔离构建与补丁

用户选定 repo 后记录 mount_id、根目录身份、允许读取范围和 baseline 文件摘要。执行 build/test 属于运行仓库代码，不能因名称为“测试”就视为纯读取；依赖安装脚本、网络、凭证继承和子进程均受隔离策略控制。默认在隔离工作副本构建，只把必要结果作为 Observation 回传。

PatchSet 保存每个文件的相对路径、操作类型、before_hash、after_hash、补丁 blob_ref 和来源 execution_id。应用前再次解析路径与文件身份，确认未被用户或外部工具修改。冲突时保留用户改动并返回 PATCH_CONFLICT，不能覆盖后宣称自动修复成功。

首版建议只支持常规文本文件的受控修改；符号链接、junction、硬链接别名、NTFS alternate streams、设备路径及其他无法证明范围安全的目标先拒绝。具体支持矩阵由路径安全测试冻结，不以字符串前缀检查作为边界。

### K.2 多文件应用与验证

多个文件替换不能假装是一个文件系统原子事务。Executor 在受保护 journal 中记录每个文件 PREPARED/APPLIED/VERIFIED 及备份引用；崩溃后按 before/after hash 判断进度。回滚前仍检查文件没有被外部修改；无法安全回滚时报告部分应用，停止后续任务。

Verifier 输入是具体 Artifact 版本与 baseline，输出 PASS/FAIL/INCONCLUSIVE、检查项、环境指纹、命令引用、exit_code 和日志 blob_ref。只有所有 required checks 为 PASS 才成功。测试进程未运行、日志丢失、超时或跳过必需测试都不是 PASS。LLM 判断只能补充无法确定性验证的项目，不能覆写编译失败。

### K.3 一条最小验收用例

准备带固定失败测试的小仓库，生成 baseline 和预期失败证据。Agent 读取并产生补丁，在隔离副本运行测试；批准后以匹配的摘要写回，再对最终字节执行验证。分别在补丁准备后、Grant claim 后、文件替换后、结果回传前杀进程；恢复后的 diff、Artifact hash、ledger 和测试结果必须一致。


# 09 · 安全架构、权限系统与 Full Access

## 文档目的
DeepSeek Harness 的引入不能弱化 R2 最重要的安全差异化：模型、Agent Runtime、Skill、MCP、Web、Parser 都是潜在攻击面。Trusted Authority Core 保持最小 TCB、独立 Rust 进程和窄 IPC。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| SEC-001 | Independent Authority | Release 必须独立进程。 |
| SEC-002 | Canonical request binding | Approval 绑定 canonical_request_hash。 |
| SEC-003 | Credential Broker | 模型只能请求 use-by-reference。 |
| SEC-004 | Fail closed | Authority 不可用时高风险动作失败。 |
| SEC-005 | Full Access | 只是 Policy Mode，不关闭 Authority。 |
| SEC-006 | DSH permissions | 只作为 UX/预筛选层，不是安全最终边界。 |

## 1. Threat Model
攻击来源包括恶意网页/文档、prompt injection、Community Skill、MCP Server、被劫持 Provider、模型幻觉、Agent Runtime compromise、远程设备重放、路径替换/符号链接、Credential 泄露、重复 side effect。Authority 假设 Intelligence Plane 会被完全控制，因此只相信自身 canonicalization 与已验证 IPC identity。

## 2. CapabilityRequest
请求方只提供 action claims 与参数；Authority 自己计算 canonical resource、risk、scope、policy revision。ApprovalIntent 显示给用户的是 canonicalized 结果；批准后签发短 TTL/max_uses/nonce Grant。执行时再次比对 request_hash，防止“批准 A 执行 B”。

## 3. DSH Tool 安全
DSH tool-bash/tool-pwsh/tool-fs 若用于 KYNXA Release，必须替换/包装为 Authority-aware adapter；不能让 agent-loop 直接拿宿主 cwd 与环境变量执行。可以保留 sandbox 版本用于低风险隔离任务。

## 4. Credential
Secret 存在 OS Credential/Authority 管理域；Host/DSH/Skill 只保存 CredentialRef。Provider 调用由受控 network adapter 注入短期 Secret，日志、Session、Prompt、Tool result 均不得包含完整 Key。

## 5. Full Access
Full Access 可以减少交互批准，但依旧保留 Scope、canonicalization、audit、kill switch、L4/Financial boundary、credential policy 和 revoke。用户可配置 Work/Device/Capability 限定，不存在“万能绕过开关”。

## 6. Prompt Injection Defense
安全依赖代码层隔离而非单纯 Prompt。网页/文档标记 untrusted provenance；Context Compiler 不把外部内容放在 system 权威区；工具最小化 exposure；AgentDojo/Promptfoo 等测试用于回归攻击成功率。

## Definition of Done
- Agent Runtime compromise 不能写 authority.db
- Approval 参数替换被拒绝
- Credential 不出现在 Session/日志
- Prompt Injection 不能直接产生高风险执行
- Full Access 仍可审计/撤销

### Risk Level
L0 纯读取/无敏感影响；L1 可逆本地变更；L2 较大范围文件/进程/网络动作；L3 账号、发布、外部发送、高影响系统操作；L4 金融、身份、高价值不可逆动作。Full Access 可减少 L1-L3 的确认，但不能把 L4/Secret/Audit/Kill Switch 关闭。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | 独立 Rust Trusted Authority Core |
| OWN-02 | Capability Broker、Policy Engine、Approval Validator、Grant Manager、Credential Broker、Audit、Financial hard boundary |
| OWN-03 | L0-L4 风险、Scope/Duration/Budget、Full Access policy mode |
| OWN-04 | OS-level isolation、Named Pipe ACL/identity/nonce/replay protection |
| OWN-05 | side-effect ledger 与 kill switch |

### A.2 明确不拥有
- Planner 意图
- DSH session
- 业务 GUI 状态

## B. 核心对象目录

Policy；CapabilityRequest；CanonicalAction；ApprovalIntent；AuthorizationGrant；CredentialRef；AuditEvent；SideEffectRecord；DeviceIdentity。

## C. 服务职责目录

kynxa-authority.exe；PolicyEngine；CapabilityBroker；ApprovalService；GrantManager；CredentialBroker；AuditLedger；SideEffectCoordinator。

## D. 主流程与状态推进
1. Tool Intent→canonicalization→policy evaluate
2. 低风险内建 Grant 或生成 ApprovalIntent
3. 用户批准绑定 request_hash/policy_revision/graph_revision
4. Executor claim Grant→执行→commit SideEffectRecord
5. UNKNOWN 副作用进入 query/reconcile，禁止自动重复执行

## E. 不变量与安全要求
- Authority 默认无普通 Internet egress
- Secret 不进入模型/普通 DB/日志
- Full Access 不关闭 L4/credential/audit/kill switch
- 所有远程审批与本机审批共享同一一次性决议语义
- Host/Node/Python 进程即便 RCE 也不应直接获得高权限资源

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | Authority unavailable→fail closed |
| F-02 | Grant expired/claimed→拒绝 |
| F-03 | canonicalization 无法确定→拒绝 |
| F-04 | credential backend locked→需要用户解锁 |
| F-05 | audit 写失败→高风险动作不继续 |

## H. 指标目录

approval_required_total；grant_denied_total；grant_claim_conflict_total；credential_access_total；audit_write_fail_total；security_violation_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 恶意网页诱导上传 SSH key→拒绝 |
| T-002 | 批准后修改金额→旧批准失效 |
| T-003 | Full Access 下金融动作仍需硬边界 |
| T-004 | 模拟 Host RCE 不能直接读 Credential |
| T-005 | remote device revoke 后立即失去审批权 |

## J. 实施顺序
1. 先实现 Capability/Grant/Audit 最小闭环
2. 再加 Credential/Device/Financial
3. 最后做系统化红队与 AgentDojo/Promptfoo 回归

## K. 可审查的授权与执行状态机〔建议〕

### K.1 独立进程不等于完成隔离

Authority 用 Rust 和独立进程实现是冻结要求，但同一 Windows 用户下的多个普通进程可能仍共享文件和凭证访问权。仅设置数据库文件 ACL 给同一用户，不能证明被攻陷的 Host/Node 无法读取。Release 前必须验证实际 token、账户/服务身份、受限进程、IPC 身份及资源 ACL 的组合。

本版不提前选定某一种 OS 隔离方案。P0 spike 必须用真实受攻陷客户端尝试绕过 IPC 直接读安全库、访问 Credential、启动进程和出网。任何未被隔离的能力不得宣传为“Host RCE 下安全”；缺口记录到发布范围，并禁用依赖该保证的高风险功能。

### K.2 CanonicalAction 与摘要绑定

Authority 从固定 schema 构造 CanonicalAction：action、subject、scope、canonical resource、typed parameters、content_hash、graph/policy/scope revision、credential_ref、expiry constraint。risk 由 Authority 计算，客户端声明仅作为输入参考。未知字段、重复 JSON key、NaN、超长值和类型不匹配均拒绝。

建议先完成类型化解析和资源规范化，再用固定 canonical JSON 编码形成摘要；编码版本进入 domain separator。摘要覆盖实际执行的字节摘要、endpoint/method 与授权所需字段。不能任意排序数组、改写大小写敏感路径或 Unicode 规范化文件名。相同语义跨语言编码必须有黄金向量；路径身份由 Authority 的 Windows 资源解析决定。

ApprovalIntent 展示摘要对应的可读动作、资源和影响；批准返回 intent_id + digest + expected_revision。审批卡不可只显示模型自己提供的描述。Grant 引用最终 CanonicalAction，执行器根据该对象取参数，避免在批准之后继续读取可变 UI 表单。

### K.3 Approval 与 Grant

| 对象 | 状态 | 允许的推进 |
|---|---|---|
| ApprovalIntent | PENDING | APPROVED / DENIED / EXPIRED / INVALIDATED |
| AuthorizationGrant | ISSUED | CLAIMED / EXPIRED / REVOKED |
| AuthorizationGrant | CLAIMED | CONSUMED；或保留未决执行引用 |
| SideEffectRecord | PREPARED | SENT / FAILED_NO_EFFECT |
| SideEffectRecord | SENT | CONFIRMED / UNKNOWN / FAILED_NO_EFFECT（需证据） |
| SideEffectRecord | UNKNOWN | CONFIRMED / FAILED_NO_EFFECT / MANUAL_REVIEW |

Grant 建议首版限定 max_uses=1，以缩小并发语义。ClaimGrant 在同一 Authority 事务内检查身份、nonce、deadline、digest、revision、revocation 和执行范围，原子更新状态并创建 execution receipt。并发 claim 只有一个获新执行权；同 execution_id 的网络重传可读回原 receipt，但不能触发第二次执行。

Grant 一旦被 claim，不因超时或 Node 重启恢复为 ISSUED。若能证明尚未执行，终结原执行记录并重新评估是否签发新 Grant，留下审计；不可复用旧批准去执行变更后的参数。CONSUMED 表示能力已使用，不代表任务成功。

状态表中的 FAILED_NO_EFFECT 是原 FAILED 的建议细分，只能在有证据证明没有效果时使用；部分效果或结果不明进入 UNKNOWN/MANUAL_REVIEW。Grant 状态与 ledger 状态分离：能力已消费的同时，副作用结果仍可能未知。

### K.4 副作用意图先落盘与 UNKNOWN

Executor 只能接受有效 claim receipt，先持久化执行意图，再开始外部动作；“SENT”表示已进入可能生效的区间，不等于已确认发送成功。在 SENT 落盘与真正执行之间崩溃也可能造成保守 UNKNOWN，代价是需要核实，不能为减少 UNKNOWN 把持久化放到执行之后。

CONFIRMED 要有可信执行器或受控外部查询的证据。Agent 自称成功不能提交安全事实。对于文件替换可核对 before/after hash 和 journal；外部订单需按 provider idempotency key 查询。缺少查询能力则转 MANUAL_REVIEW，并明确可能已发生的影响。补偿操作本身也需要新授权，不能用“回滚”名称绕过权限。

撤销会阻止尚未开始及后续动作，但无法保证撤回已到达外部系统的请求。Kill Switch 的验收应区分停止新调度、停止本地执行器和核实外部在途动作的时间，而不是只测试按钮是否变色。

### K.5 Credential 与审批通道

ResolveCredentialRef 在普通 Host/Runtime 契约中仅返回可使用句柄和元数据，不返回 Secret。真正注入发生在受信 adapter 或受限执行器的短生命周期调用中，拒绝跨 endpoint/subject 重用。对日志、异常、HTTP 重定向、崩溃转储和 Session 持久文件均做泄露测试。

审批提交与普通 ToolIntent 使用不同身份能力，Host 不能伪造“用户已点击批准”。本机 UI 和已配对远端控制端都需通过受信来源证明；UI 被攻陷是否在威胁模型内应单独声明。传输可达、设备配对、Controller role 和具体动作批准是四个不同检查。


# 10 · Resident、Android 远程控制、设备配对与统一审批中心

## 文档目的
R3 的 Remote 语义保持：Android 是控制平面，不是第二套 Agent Runtime。PC Host 是任务状态单一权威；手机负责远程发指令、查看进度、审批、Pause/Stop 和可选屏幕观察。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| REM-001 | 一次扫码配对 | PairingSession 短 TTL、单次消费。 |
| REM-002 | Disconnect ≠ Unpair | 断线不删除设备身份；撤销必须显式。 |
| REM-003 | Transport identity ≠ Authority permission | 进了网络不代表有控制权。 |
| REM-004 | Android userspace transport | 不抢占用户 VpnService。 |
| REM-005 | 多设备 | 手机 Controller、平板 Viewer 可并发且权限不串用。 |

## 1. Resident
Resident 只负责轻量可达、Host lifecycle、pairing/transport；不加载大模型、不持有 Agent 工具权限。收到远程 Command 后唤醒 Host，由 Host 创建/查询 Task。

## 2. Device Registry
每个设备有 device_id/public key/role/status/last_seen/revocation revision。Authority 验证 DeviceAuth 后再处理远程审批；RemoteTransport peer ID 只是网络事实。

## 3. 远程事件流
Command、Task Events、Approval、Screen Stream 分通道/优先级，Stop/Pause 不应被视频流阻塞。重复 RemoteCommand 使用 request_id 去重，返回原 Task 状态而不是创建第二任务。

## 4. 网络切换
手机 Wi-Fi↔5G、PC VPN/校园网变化后自动重连，不要求重复扫码。Direct 优先、Relay fallback；第一版验收以“可用”优先，不以必须 P2P 或高画质为门槛。

## 5. DSH 关系
手机不直接操纵 DSH Session。它只发 KYNXA Command/Task action，Host 决定是否 DeliverPrompt/CancelTurn/ResumeSession。这样 Remote 与内部 Agent Runtime 解耦。

## Definition of Done
- 真实跨网配对与重连
- 重复命令不重复执行
- 手机审批 request_hash 正确绑定
- Agent Runtime 更换不影响设备配对
- Stop 通道低延迟

### 多设备角色
`CONTROLLER / VIEWER / MIXED` 是 Session role，不是永久设备特权。Phone 可作为 Controller，Tablet 可同时作为 Viewer。Direct Input 单独授权；Screen View 与 Agent Command 分离。任何远程 Approval 仍绑定 Authority 的 canonical action digest。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | Windows Resident 最小常驻、Host start/wake/health |
| OWN-02 | Android Control Plane、QR 一次性配对与设备密钥 |
| OWN-03 | RemoteSession role/permission |
| OWN-04 | 远程 Task 指令、状态、审批与可选 Screen stream |
| OWN-05 | 设备撤销与多设备并发 |

### A.2 明确不拥有
- 模型推理常驻
- 任意远程桌面软件替代
- Authority policy

## B. 核心对象目录

Device；PairingIntent；RemoteSession；RemoteCommand；RemoteApproval；ScreenStreamSession；ResidentHealth。

## C. 服务职责目录

kynxa-resident；Android Client；DeviceRegistry；PairingService；RemoteGateway；ScreenStreamWorker。

## D. 主流程与状态推进
1. PC Resident 等待已配对设备→认证→可启动 Host
2. QR pairing 只在短时窗口有效并交换设备公钥
3. Phone Controller 与 Tablet Viewer 可建立独立 session
4. 远程 approval 最终提交到同一 Authority ApprovalIntent
5. Screen worker 按需启动，session 结束退出

## E. 不变量与安全要求
- 网络可达不等于 KYNXA 授权
- Resident 不加载模型、不保存原始 Credential
- Viewer 默认只读，Controller 也不自动获得 direct input
- 设备撤销立即使后续 token/session 无效
- 远程命令必须带 device/session identity

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | Host 未运行→Resident start host |
| F-02 | 网络切换→session 重连但不重复执行 side effect |
| F-03 | 设备时钟漂移→nonce/sequence 而非仅靠时间 |
| F-04 | Screen encoder crash→控制通道仍可用 |
| F-05 | pairing code 重放→拒绝 |

## H. 指标目录

resident_idle_mb；remote_connect_ms；pairing_success_rate；remote_command_latency_ms；screen_fps；revoked_session_reject_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 手机 5G + 平板热点同时连接 PC |
| T-002 | 撤销设备后旧 session 失效 |
| T-003 | Host crash 后手机可请求重启并恢复 task |
| T-004 | 远程审批与 PC 并发只消费一次 Grant |
| T-005 | Viewer 不能发送控制命令 |

## J. 实施顺序
1. 先 pairing+task status+approval
2. 再 host lifecycle
3. 最后 screen view/direct input 可选能力

## K. 远程命令、配对与撤销细化〔建议〕

PairingIntent 绑定 PC 公钥指纹、临时 nonce、有效期和单次消费标志。手机扫码后双方确认同一配对会话；二维码泄露不能长期提供控制权。设备长期身份保存公钥和撤销 revision，私钥留在设备安全存储，不把传输 peer ID 当授权身份。

RemoteCommand 携带 device_id、remote_session_id、request_id、sequence、payload_digest 和目标 scope。Host 对 device/session 校验后，以 request_id 去重；重复请求返回原 task_ref。相同 request_id 不同 digest 必须拒绝。sequence 防重放不替代请求去重，断线重连需显式协商游标。

撤销先提交 Authority 安全状态，再使后续请求失效；Host 缓存只可缩短有效期，不可延长已撤销设备权限。已发生的副作用不能因设备撤销而消失。屏幕流、事件流和控制命令采用独立队列；Stop/Pause 的延迟在大文件传输和弱网下单独测量。


# 11 · Agent Wallet、购物 Skill 与 Agent Commerce

## 文档目的
Commerce 保留既有边界：KYNXA 不托管用户资金，模型不能直接完成不可逆支付。DSH 可以帮助搜索、比较、准备订单，但最终 Financial Boundary 由 Authority 与支付 Provider adapter 约束。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| COM-001 | No custody | KYNXA 不持有资金。 |
| COM-002 | PaymentIntent | 支付前形成明确金额/商家/订单/币种/收货等 intent。 |
| COM-003 | Financial hard boundary | 模型/Skill/Full Access 不能自动跨越。 |
| COM-004 | Reconciliation | 网络超时 UNKNOWN 时查询而非重复扣款。 |

## 1. Shopping Skill
DSH Skill 可负责任务拆分、商品检索与比较，Search/Browser Tool 受 Network/Authority policy。结果卡必须区分广告/来源/价格时间戳/币种。

## 2. Payment Flow
Agent 只能提出 PaymentIntent；用户在受信 UI 确认 canonicalized 金额和商家；Authority 签发一次性 Transaction Grant；受控 adapter 调起官方支付路径。模型不接收银行卡/CVV/支付密码。

## 3. Side Effect
下单/支付属于 non-idempotent。SENT 后超时进入 UNKNOWN，恢复流程先 query/reconcile。Replay 永远用 recorded result/mock，不能真实扣款。

## Definition of Done
- 支付信息不进 Prompt
- 重复网络请求不重复扣款
- Full Access 不绕过 Financial boundary
- 订单/支付状态可 reconciliation

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | 商品比较/购物卡/订单意图 |
| OWN-02 | PaymentIntent 与 Financial Policy |
| OWN-03 | 不托管资金原则 |
| OWN-04 | 支付认证与多设备批准 |
| OWN-05 | Outbox/Reconciliation、Skip Today、预算与商家策略 |

### A.2 明确不拥有
- 银行清算
- 保存银行卡明文
- 让模型直接调用支付凭证

## B. 核心对象目录

ProductCandidate；ShoppingPlan；OrderIntent；PaymentIntent；MerchantProfile；FinancialPolicy；PaymentSideEffect。

## C. 服务职责目录

ShoppingSkill；CommerceAdapter；PaymentCoordinator；MerchantVerifier；FinancialPolicyEngine(Authority-owned parts)。

## D. 主流程与状态推进
1. 检索商品→生成候选与证据→用户/策略选择
2. 创建 OrderIntent→最终金额/商家/商品 canonicalize
3. PaymentIntent 进入 Authority L4 hard boundary
4. 外部支付结果不确定→reconciliation 查询
5. 拒绝可进入 Skip Today/替代商品/结束

## E. 不变量与安全要求
- 模型永远拿不到可复用支付 secret
- 价格/数量/收货地址变化使批准失效
- 不得把“已发送请求”误报为“已成功支付”
- 自动购买只在明确预算/商家/品类/上限 scope 内
- 默认保留可审计订单与理由

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | 价格变化→重新确认 |
| F-02 | 库存消失→重新计划 |
| F-03 | 支付网关超时→UNKNOWN |
| F-04 | 商家身份无法确认→阻断 |
| F-05 | 多设备重复确认→只一次成功 |

## H. 指标目录

commerce_plan_to_order_rate；price_change_reapproval_total；payment_unknown_total；reconciliation_latency_ms。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 批准后金额被篡改→拒绝 |
| T-002 | 支付超时不二次扣款 |
| T-003 | 预算超限→Authority 阻断 |
| T-004 | 模拟商家/沙盒环境端到端测试 |
| T-005 | 购物推荐明确区分事实、偏好和广告/推广来源 |

## J. 实施顺序
1. 先只做比较/购物清单
2. 再沙盒订单
3. 最后受控真实支付集成

## K. 金融边界的落地前置条件〔建议〕

当前切片仅保留商品比较和沙盒订单，不把真实支付接入视为 Agent 完成前提。启用真实支付前，必须确认 Provider 支持的幂等和订单查询语义、商家身份、币种小数位、认证方式及退款/撤销限制，逐项记录到 adapter compatibility matrix。

最终批准摘要覆盖商家标识、商品/数量、最终金额/币种、税费/运费、收货地址引用及订单版本；任一变化使批准失效。支付 token 绑定商家、金额和有效期；模型不处理可复用支付 Secret。Full Access 不能绕过 L4，自动化预算许可也不等于单笔支付证明。

支付超时后保持 UNKNOWN，按同一 provider operation key 查询。没有查询能力时停止并提示人工核实；不能以新 key 再次扣款。对外展示“请求已提交”“支付已确认”和“核实中”三个不同状态，验收包含重复 webhook、乱序回执、重复批准和重启后的核实。


# 12 · Routine、Intent Subscription 与个人自动化

## 文档目的
Routine 是持久触发器，不是把 DSH agent-loop 永久在线。Scheduler 属于 Host；触发后创建 Task，再按需启动 DSH Session。这样可控资源、权限和恢复。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| AUTO-001 | Trigger ownership | 时间/事件/条件触发由 Host Scheduler 管理。 |
| AUTO-002 | Task per run | 每次 run 产生独立 task/run_id。 |
| AUTO-003 | Budget/permission | 自动化有单独预算与授权上限。 |
| AUTO-004 | No hidden persistence | DSH Session 结束不等于 Routine 消失；Routine 真相在 KYNXA。 |

## 1. Trigger
支持 cron/timezone、文件事件、网络状态、Work 事件、条件轮询与用户意图订阅。睡眠/休眠后的 missed run 按策略 catch-up/skip，不盲目集中补跑。

## 2. 执行
Run 创建 TaskNode，必要时绑定 DSH Session。读取类自动化可低交互；写入/发送/支付仍受 Authority policy。可为 Routine 配置“仅建议、需确认、允许某范围自动执行”。

## 3. 去重与失败
同一个 scheduled occurrence 有 deterministic run key；崩溃恢复不重复创建。连续失败触发 backoff/pause/inbox，而不是无限 agent retry。

## Definition of Done
- 时区/休眠测试
- 重复触发去重
- 自动化权限不超过预设范围
- 失败会停而非无限烧 Token

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | Cron/事件/条件/意图订阅统一触发模型 |
| OWN-02 | RoutineRun 状态机、去重、Snooze/Pause/Skip |
| OWN-03 | 上下文感知自动化与预算 |
| OWN-04 | 休眠/时区/DST/错过运行 |
| OWN-05 | 自动化的权限与通知策略 |

### A.2 明确不拥有
- 任意后台无限循环 Agent
- 自动授予新权限
- 绕过用户 quiet hours

## B. 核心对象目录

Routine；Trigger；IntentSubscription；RoutineRun；RunBudget；NotificationPolicy；DedupKey。

## C. 服务职责目录

RoutineScheduler；TriggerEngine；IntentMatcher；RunCoordinator；NotificationService。

## D. 主流程与状态推进
1. Trigger 命中→创建唯一 Run→加载 scope/context→执行 deterministic/agent node
2. 高风险动作仍进入 Approval
3. 失败按策略 retry/backoff 或等待用户
4. 休眠后按 catch-up policy 决定补跑/跳过
5. 重复事件通过 dedup key 合并

## E. 不变量与安全要求
- 自动化不能扩大原授权 scope
- 条件 watch 最高频率/资源预算受限
- 长期运行必须有可暂停/停止/审计入口
- Quiet hours 默认抑制非紧急打扰
- Intent Subscription 解释 why triggered

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | 系统休眠→恢复时 reconciliation |
| F-02 | 时区变化→按 schedule semantics 重算 |
| F-03 | 同一事件多次到达→幂等去重 |
| F-04 | 模型不可用→可降级 deterministic path 或延迟 |
| F-05 | 通知通道失败→保留 Inbox item |

## H. 指标目录

routine_runs_total；dedup_suppressed_total；missed_run_total；run_latency_ms；user_snooze_rate。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | DST 切换不重复关键动作 |
| T-002 | PC 睡眠后按策略补跑 |
| T-003 | 重复 webhook 只创建一项 Run |
| T-004 | Routine 无权限时进入 Needs You 而非静默失败 |
| T-005 | 用户 Pause 后不再触发 |

## J. 实施顺序
1. 先本地 schedule
2. 再事件/条件
3. 最后 Intent Subscription 与多设备控制

## K. 调度发生项与执行去重〔建议〕

Routine 定义保存 schedule_version、timezone、missed_run_policy、quiet_hours、预算和权限 scope。每个 occurrence 由 routine_id、schedule_version 与计划 UTC 时间构成唯一键；DST 重复本地时间映射到不同 UTC instant，再按明确政策选择一次或两次。不能仅使用“日期+小时”去重。

在事务内写入 occurrence 和 Task/outbox；若已存在则返回已有 Run。睡眠恢复后先计算漏跑集合，再按 skip、run_once_latest 或 bounded_catch_up 策略处理。修改计划产生新版本，但需记录已承接的 occurrence，避免旧新版本交接重复。

建议首版同一 Routine 禁止重叠执行，前一次未结束时跳过或等待并记录原因。Pause 阻止新 occurrence，不自动撤销已经发出的副作用；Stop 当前 Run 复用 Task 取消/核实语义。连续失败达到阈值进入 Needs You，阈值作为显式配置，不写死成无限重试。


# 13 · 个人 Agent 体验、偏好学习、Shadow Mode 与 Agent Constitution

## 文档目的
长期个性化仍由 KYNXA 保存结构化偏好和可撤销规则，不交给某个模型 Session。DSH 只在 ContextBundle 中接收当前允许使用的偏好摘要。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| PERS-001 | Structured preference | 偏好有 scope/source/confidence/revision。 |
| PERS-002 | Shadow Mode | 先观察/建议，不自动执行副作用。 |
| PERS-003 | Constitution | 用户定义长期行为原则，但不能放宽 Authority 硬边界。 |
| PERS-004 | Forget | 用户可查看/删除长期偏好与记忆。 |

## 1. Preference
区分显式设置、用户反馈推断、Work 专属偏好与临时上下文。低置信推断不能静默变成硬规则。模型输出“我认为你喜欢…”必须先形成 candidate。

## 2. Constitution
Constitution 影响语气、工作偏好、探索度、汇报方式和是否先询问，但不改变网络、文件、Credential、支付等 Authority policy。

## 3. Shadow
Shadow Mode 让系统在不执行的情况下记录“如果允许我会怎么做”，用于评估工具选择与风险。Shadow trace 可以喂给 benchmark/replay，但不能产生真实 side effect。

## Definition of Done
- 删除偏好后不再进入 Context
- Constitution 不影响硬权限
- Shadow 无副作用
- Work 偏好不泄漏到其他 Work

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | 结构化偏好与 Agent Constitution |
| OWN-02 | Teach KYNXA、Why Asking、Shadow Mode |
| OWN-03 | Do-it-and-tell-me/Inbox/每日简报 |
| OWN-04 | 偏好证据、置信度、可撤销与可忘记 |
| OWN-05 | 探索度与个性化边界 |

### A.2 明确不拥有
- 敏感属性推断
- 隐藏式政治/商业操纵
- 把偏好当权限

## B. 核心对象目录

Preference；PreferenceEvidence；ConstitutionRule；ShadowSuggestion；UserFeedback；BriefingItem。

## C. 服务职责目录

PreferenceService；ConstitutionEvaluator；ShadowModeService；FeedbackRouter；BriefingComposer。

## D. 主流程与状态推进
1. 用户显式 Teach 或从重复行为生成低置信候选
2. 候选先在 Shadow Mode 观察，不直接自动执行
3. 用户确认后提升 preference confidence
4. 冲突偏好按 scope/time/source 解析
5. Why Asking 展示触发规则与当前假设

## E. 不变量与安全要求
- Preference != Permission
- 敏感个人属性默认不推断/不固化
- 用户可查看、编辑、忘记偏好来源
- 失败反馈优先修正行为，不强行解释用户
- 个人化不能覆盖 Authority/Privacy 硬边界

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | 偏好冲突→降级为询问或 scope-specific rule |
| F-02 | 低置信候选→保持 shadow |
| F-03 | 用户撤销→后续 context 不再注入 |
| F-04 | 摘要过度个性化→可关闭/回到中性模式 |
| F-05 | 数据删除后 derived cache 同步清理 |

## H. 指标目录

shadow_accept_rate；preference_reversal_rate；why_asking_open_rate；briefing_dismiss_rate。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 用户修改偏好立即影响后续 task |
| T-002 | 忘记偏好后不再出现 |
| T-003 | Shadow suggestion 不产生真实 side effect |
| T-004 | Work 特定偏好不泄漏到其他 Work |
| T-005 | 偏好不能替代审批 |

## J. 实施顺序
1. 先 explicit preference
2. 再 shadow learning
3. 最后 briefing/constitution 与长期治理

## K. 偏好治理与 Shadow 的可验证边界〔建议〕

偏好记录 value、scope、source、evidence_ref、confidence、revision、status 和可选 expires_at。显式用户设置优先于推断；Work 偏好只在对应 scope 注入。用户删除后立即停止新 ContextBundle 使用，异步清理派生缓存，并提示已发送到外部 Provider 的内容不能被本地删除操作撤回。

Shadow Mode 在执行网关层强制拒绝真实副作用，并用模拟 Observation 驱动后续步骤，不能仅依赖 Prompt 说“不要执行”。只读观察也需原有 scope 和网络策略。候选行为记录预期动作与可用证据，评估误报、接受和撤销率；单次接受不足以升级为自动执行权限。

Constitution 和偏好都只是上下文约束，不写入 Authority Policy。若用户明确要求改变权限，需要走独立的策略编辑与授权路径，并展示变化范围。


# 14 · 缓存、存储、快照、回放、可观测性与灾难恢复

## 文档目的
R3 新增最关键的数据边界是 DSH Session Persistence 与 KYNXA Durable State 并存。两者互补，不互相取代。可重建索引/缓存损坏不能导致 Work/Authority 真相丢失。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| STOR-001 | kynxa.db | 业务 Source of Truth。 |
| STOR-002 | authority.db | 安全 Source of Truth，Host/DSH 不直写。 |
| STOR-003 | DSH session persistence | Agent Session 事实与恢复；默认/首选使用上游持久后端策略。 |
| STOR-004 | Derived indexes | FTS/vector/session query 可重建。 |
| STOR-005 | Checkpoint ≠ Cache | checkpoint 是恢复语义，cache 可丢。 |

## 1. Data Ownership
Work/Task/CognitiveState/Artifact 元数据在 kynxa.db；Grant/Approval/Policy/Audit 在 authority.db；文件字节在 Blob Store；DSH Session 的 model/tool turn 在 session persistence；session-query/FTS/vector 是 projection/index。禁止创建第五套不明真相源。

## 2. Checkpoint
Checkpoint 保存 Task graph revision、current node、SessionBinding、critical Observation refs、Artifact refs、side effect refs、environment snapshot 与 resource state。模型生成到一半不做 token 级 checkpoint；只在已提交事实边界恢复。

## 3. Replay
Replay 可选择只重放 DSH Session/model turns、或者重放 KYNXA Durable Task。non-idempotent side effect 默认使用 recorded Observation/mock；只读工具可配置真实执行。用于 DSH 版本升级回归与 Skill promotion。

## 4. Flight Recorder
记录 task/session/model/tool/subagent/workflow/authority/resource/latency/repair/recovery 事件。默认对敏感 Prompt/Tool payload 仅记录 hash/引用；Developer Mode 可在本机展开。

## 5. Disaster Recovery
Host crash：SQLite WAL + checkpoint；Agent Runtime crash：重启并恢复 Session；Model crash：重载并重新调用；索引损坏：重建；cache 损坏：丢弃；disk pressure：先清临时/cache/build，再提示模型空间，最后才在用户策略下清旧 Artifact/Snapshot。

## Definition of Done
- 清 Cache 不丢任务
- 删除 session query index 可重建
- kill/restart 不重复副作用
- 业务库损坏不等于安全库可写
- 恢复指标可观测

### 真相层次
1. KYNXA 业务真相：`kynxa.db`；2. 安全真相：`authority.db`；3. 文件真实字节：workspace/artifact/blob store；4. DSH Session execution history：session persistence；5. 可重建索引/缓存：FTS/vector/session-query/cache。恢复顺序严格按不可重建程度从前到后。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | Cache/Reuse 与业务真相分离 |
| OWN-02 | Checkpoint/Snapshot/Event/Flight Recorder |
| OWN-03 | Replay 与 side-effect mock/reconciliation |
| OWN-04 | Crash recovery、backup、restore、migration |
| OWN-05 | 日志/trace/metrics 的隐私与采样 |

### A.2 明确不拥有
- KYNXA 业务 schema 本身（见 17）
- Authority policy 本身
- 模型算法

## B. 核心对象目录

CacheEntry；Checkpoint；Snapshot；EventRecord；TraceSpan；BackupManifest；RecoveryPlan。

## C. 服务职责目录

CacheService；CheckpointStore；SnapshotManager；FlightRecorder；BackupService；RecoveryCoordinator。

## D. 主流程与状态推进
1. deterministic cache 按 input hash/model/tool/version/scope 键控
2. 任务关键节点写 checkpoint
3. 定期/升级前生成 snapshot/backup manifest
4. Crash 后先恢复 truth store，再重建 derived index/cache
5. Replay 默认替换外部副作用为 recorded result/mock

## E. 不变量与安全要求
- Checkpoint != Cache
- Cache 可删可重建，不得承载唯一事实
- Replay 不真实重复支付/发送/删除
- Trace 默认不记录 secret/full sensitive prompt
- 备份与恢复必须包含 schema/version/manifest 校验

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | cache corruption→丢弃重建 |
| F-02 | snapshot 不完整→拒绝标记成功 |
| F-03 | 恢复时版本不兼容→migration/只读模式 |
| F-04 | 日志磁盘占满→降级采样且保护业务库 |
| F-05 | Flight Recorder 关闭时核心审计仍由 Authority 保留 |

## H. 指标目录

cache_hit_rate；checkpoint_latency_ms；snapshot_size_mb；recovery_time_objective_ms；replay_success_rate；trace_drop_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 删掉所有 vector/cache 后系统可重建 |
| T-002 | task kill/restart 保持 side-effect once semantics |
| T-003 | 备份到新机器恢复 Work/Artifact 引用 |
| T-004 | Replay payment 只用 mock |
| T-005 | 磁盘满时不损坏 SQLite |

## J. 实施顺序
1. 先 checkpoint/flight recorder
2. 再 backup/restore
3. 最后 deterministic replay 与灾难演练

## K. 崩溃切点、恢复顺序与证据〔建议〕

| 崩溃位置 | 持久事实 | 恢复动作 |
|---|---|---|
| Node 领取前 | READY | 正常竞争 lease |
| Session 已创建，Host 未收到 ACK | PENDING binding + Runtime journal | 按 binding_id 查询；禁止盲建第二个 |
| Grant claim 后、执行意图前 | claim receipt | 查询 Executor；不能重用 Grant |
| 执行意图后、外部结果前 | SENT / UNKNOWN | 核实外部效果，禁止自动重复 |
| 效果已发生、ledger 未确认 | receipt/journal 或外部结果 | 验证证据后提交确认 |
| ledger 已确认、Host 未收结果 | CONFIRMED + result_ref | 补投 Observation，不重复执行 |
| Host 已提交、Session 未收结果 | outbox + observation_id | 幂等补投 Session |
| 索引更新中 | 业务事务与 outbox 已提交 | 重建/重投索引，不回滚事实 |

### K.1 恢复算法

先获得 Host 单写者锁并校验业务库，再连接 Authority 获取未决执行状态；验证必要 blob，恢复 Task/Session 投影，最后重建索引。安全库不可用时不推导 Grant 有效性。对每个未终止 execution 产生 RecoveryDecision，记录输入证据、判断、下一动作和原因，便于人工排查。

对账必须区分安全事实与业务投影：Authority 已确认不代表 Task 已通过 Verifier；业务显示失败也不代表外部副作用未发生。定期比对两侧 execution_id、状态和 result_ref，发现差异先补投事件，不先重做动作。

### K.2 备份、恢复与保留

使用 SQLite 一致性备份机制或受控停写快照，不直接复制正在写入的主 db 文件而忽略 WAL。BackupManifest 包含 schema 版本、文件摘要、时间、数据域、Session 格式、加密方式和排除项。Authority 由自身导出安全备份，Host 只记录 opaque backup_ref。

恢复到另一台机器不默认恢复活动 Grant、lease、设备会话或可用 Credential；需要重新验证机器绑定与设备授权。业务历史与 Artifact 可恢复，受机器保护的 Secret 可能需要重新输入。旧备份不能使已撤销权限“复活”；回滚恢复时必须有单独的安全再注册流程。

GC 从临时文件、可重建 cache 开始；checkpoint、未决 side effect、审计保留期内引用均作为保留根。Blob 删除使用引用检查与宽限期，防止 outbox/索引延迟造成误删。备份恢复测试要真正恢复到新数据根目录并校验引用，不能只检测压缩包存在。


# 15 · 研究价值、开源 Benchmark、实验评估与开发路线

## 文档目的
R3 的研究价值从“自研所有 Agent 组件”转向“成熟 Harness + 可验证产品 Runtime 的系统研究”：重点研究 Durable State、Authority、Model/Context/Tool/Skill 联合调度、消费级本地模型资源适配和真实桌面任务安全性。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| EVAL-001 | 桌面能力 | WindowsAgentArena / OSWorld。 |
| EVAL-002 | Agent 工具与策略 | tau-bench/tau3 类、BFCL。 |
| EVAL-003 | 安全 | AgentDojo、Promptfoo、garak。 |
| EVAL-004 | Work | TheAgentCompany 类真实工作任务。 |
| EVAL-005 | RAG | BEIR/Ragas + 自建 Work scope isolation set。 |
| EVAL-006 | R3 核心消融 | DSH upstream vs KYNXA adapters/Authority/State/Context/Resource。 |

## 1. 评估原则
不把单一模型跑分当 KYNXA 分数。必须区分 model capability、Agent harness capability、product runtime、security、RAG、desktop execution。报告任务集合、环境、模型、profile、权限策略、cold/warm cache 与成本。

## 2. KYNXA-Eval Harness
统一 adapter 驱动 WindowsAgentArena/OSWorld/AgentDojo/ToolCalling/RAG 测试，记录 Task Success、Tool Selection Accuracy、Illegal Tool Rate、Authority Block/False Block、Prompt Injection ASR、Steps、Tokens、TTFT、Wall Time、Recovery Success、Loop Rate、RAG Recall/faithfulness。

## 3. DSH 相关消融
A：纯 DSH baseline；B：DSH + KYNXA Context；C：+ Durable Task/CognitiveState；D：+ Authority；E：+ Model/Resource Policy。这样可以证明增益来自哪些 KYNXA 层，而不是把上游能力当原创。

## 4. 开发路线
Phase 0：adapter/protocol；Phase 1：Fast Chat；Phase 2：CodeRepair Vertical Slice；Phase 3：Knowledge/Preview；Phase 4：Remote/Approval；Phase 5：Benchmark；Phase 6：community Skill/MCP。每阶段先打通最小闭环，再扩能力。

## Definition of Done
- 基准配置可复现
- 安全与能力分开报告
- 至少有 kill/restart/side-effect 测试
- 不夸大 DSH upstream 为 KYNXA 原创
- 每次 DSH 升级跑固定 regression set

### KYNXA-Eval 推荐覆盖
- WindowsAgentArena / OSWorld：真实桌面与多应用任务。
- AgentDojo / Promptfoo / garak：prompt injection、工具安全和红队回归。
- BFCL / τ 系列：function/tool calling、多轮策略与业务规则。
- BEIR/RAGAS 类：检索与 RAG。
- 自建 Durability Suite：kill/restart、session generation、side-effect UNKNOWN/reconciliation、Work scope 隔离。
每类结果单独报告，禁止合成一个“总分”掩盖安全或可靠性退化。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | 研究问题与不夸大创新边界 |
| OWN-02 | 公开 Benchmark 与自建 KYNXA-Eval |
| OWN-03 | 任务成功、工具调用、安全、恢复、资源效率指标 |
| OWN-04 | ablation/seed/置信区间/可复现规范 |
| OWN-05 | 开发路线与 release gate |

### A.2 明确不拥有
- 为了论文增加核心模块
- 只报告 cherry-picked case
- 把第三方能力宣称为自研创新

## B. 核心对象目录

BenchmarkSuite；EvalCase；RunConfig；EvalResult；Ablation；RegressionBaseline；ReproManifest。

## C. 服务职责目录

KYNXA-Eval Harness；Benchmark Adapters；Result Store；Regression Dashboard。

## D. 主流程与状态推进
1. 固定版本/模型/seed/environment→执行 benchmark→记录 trace/metric
2. 对 OS/Agent/Tool/RAG/Security 分层评估
3. 每次 runtime/upstream 升级与 baseline 做回归
4. 失败 case 自动分类为 model/tool/authority/context/recovery
5. 研究实验与产品 telemetry 严格区分

## E. 不变量与安全要求
- 必须区分事实、外部 benchmark 结果和工程判断
- 安全 benchmark 不以降低安全边界换成功率
- 比较时明确模型、硬件、时间、版本
- 统计结果报告样本数/波动/失败定义
- 未复现第三方结果不作为内部已证实结论

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | benchmark 环境漂移→标 invalid |
| F-02 | 模型 API 版本变化→新 baseline |
| F-03 | 隐藏测试失败→保存完整 trace |
| F-04 | 资源不足→按预定义降级，不能临时改 metric |
| F-05 | 第三方 benchmark license/数据不可用→替代或跳过 |

## H. 指标目录

task_success_rate；tool_selection_accuracy；prompt_injection_asr；authority_block_rate；resume_success_rate；tokens_per_success；wall_clock_per_success；rag_recall_at_k。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | WindowsAgentArena/OSWorld 桌面任务 |
| T-002 | AgentDojo/Promptfoo/garak 安全回归 |
| T-003 | BFCL/τ-bench 类工具/策略任务 |
| T-004 | RAGAS/BEIR 类 Knowledge 测试 |
| T-005 | kill/restart/replay 自建 durability suite |

## J. 实施顺序
1. 先建立 CodeRepair internal suite
2. 再接 tool/security benchmarks
3. 最后接 desktop/long-horizon work benchmarks

## K. 首个发布切片的证据要求〔建议〕

将完成度拆成 UI 原型、可运行闭环、故障恢复、安全隔离和性能证据五项，分别报告。现有桌面 UI 不计为模型接通或 Authority 完成；文档列出的测试也不计为已通过测试。

建议 P0 内部集至少覆盖：真实失败仓库修复、审批拒绝、修改批准参数、两个 worker 争抢、执行后回执丢失、Host/Runtime kill、Session 丢失、索引重建、Strict 能力不足、云外发禁止和路径逃逸。每个 case 固定输入 fixture、注入位置、预期状态、可观察证据与通过判据，详见附录 D。

对照实验首先保持模型、任务、预算和工具能力一致，再比较 DSH 基线与 KYNXA 增量。能力成功率和安全阻断/误阻断率分别报告；恢复实验同时检查重复副作用计数。小样本只报告探索性结果，不能推导普遍优势。

性能数值先建立实际硬件基线：冷启动、首 token、流式 UI 更新、Authority 判定、checkpoint 提交、恢复耗时和内存峰值。本文不虚构已达成的毫秒数、显存占用或百分比；性能预算由测量后写入 ADR 与 CI Gate。


# 16 · 网络架构、Web Search、VPN/代理、远程连接与实时同步

## 文档目的
R3 网络层继续保持四层解耦：Model Provider ≠ Search Provider ≠ Network Route ≠ Authority Policy。DSH 自带 Web 工具只是 Agent surface，不允许直接绕过 KYNXA Network Profile 与 Authority。

## 冻结决策
| ID/主题 | 冻结结论 | 工程含义 |
| --- | --- | --- |
| NET-001 | Follow user network | 尊重系统 VPN/TUN/代理。 |
| NET-002 | Unified Proxy Resolver | Windows 各技术栈不各走各路。 |
| NET-003 | DSH Web Adapter | web-search/fetch 通过 KYNXA Network Worker/Policy。 |
| NET-004 | Remote plane separation | 设备互联与普通 Web egress 分离。 |
| NET-005 | No first-party cloud | KYNXA 不依赖自有云才能工作。 |

## 1. 网络四层
Model Provider 回答“谁推理”；Search Provider 回答“谁找网页”；Network Route 回答“数据从哪里出去”；Authority Policy 回答“允许发送什么到哪里”。切本地/云模型不应改变 Search 能力，也不能隐式改变隐私。

## 2. Windows Proxy
统一 SystemProxyResolver 解析 KYNXA Custom Proxy、HTTP(S)_PROXY/ALL_PROXY/NO_PROXY、Windows user proxy/PAC、WinHTTP、Direct，生成 ProxyPlan 给 .NET/Rust/Browser/Node worker 使用。Secret 由 Credential Broker 引用。

## 3. DSH Web
DSH web-search/fetch 注册为 KYNXA-aware provider：query/URL 先检查 network/search policy；抓取 worker 限制协议、重定向、私网地址、下载大小和 MIME；结果标记 untrusted provenance 后才进入 Session。Provider-native Search 也不得绕过 policy。

## 4. Remote Connectivity Plane
RemoteTransport 独立于 Web egress。Android 使用 userspace transport，不占系统 VpnService；Direct 优先，Relay fallback；网络变化自动迁移。Remote transport 只提供加密可达性，控制权仍需 KYNXA device auth + Authority。

## 5. Realtime
Token Stream、Agent Event、Approval、Remote Command、Screen Stream 有独立 QoS/背压。UI token 可以 16-50ms 聚合；高优先 Stop/Approval 不被大 Artifact/视频堵塞。

## Definition of Done
- 系统 VPN 开关不破坏权限语义
- Node/.NET/Rust 代理路径一致
- DSH Web 无私网 SSRF
- Remote 与 Web egress 可独立关闭
- Stop/Approval 通道在负载下可用

### 网络与模型解耦
“Local DeepSeek + KYNXA Search”是正式支持场景：本地模型不需要 Provider-native Search。反之，某云 Provider 的 server-side search 不受本机 VPN/代理路径直接控制，UI/Trace 必须明确标记该事实，避免误导。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | NetworkProfile、Direct/System/Custom proxy/PAC/WPAD |
| OWN-02 | KYNXA Web Search/Fetch 与 Provider-native search 区分 |
| OWN-03 | SSRF/private network 防护、DNS rebinding 复核 |
| OWN-04 | Resident/Remote overlay path 与实时同步 |
| OWN-05 | 网络 secret 与代理 credential |

### A.2 明确不拥有
- 模型 Provider 的服务器端网络行为控制
- 浏览器扩展代理冒充系统代理
- 把 VPN 视为权限授予

## B. 核心对象目录

NetworkProfile；ProxyPlan；SearchRequest；FetchRequest；RemotePath；ConnectionHealth；SyncCursor。

## C. 服务职责目录

NetworkResolver；SearchWorker；FetchWorker；ProxyResolver；kynxa-net sidecar；RealtimeSyncService。

## D. 主流程与状态推进
1. 请求绑定 NetworkProfile→解析 proxy/direct plan
2. 连接前解析目标并做 public/private 分类
3. redirect/DNS 变化后再次分类
4. 本地模型也可通过 KYNXA Search 获取 Web Evidence
5. 远程连接优先 direct，必要时 relay/degraded 并保留身份认证

## E. 不变量与安全要求
- Cloud model API 与本机 Search 网络路径解耦
- Skill network=false 时即使 VPN 开启也不能联网
- localhost/private subnet 默认禁止 Web Fetch
- proxy password 使用 CredentialRef
- 网络失败不能偷偷改成 policy 禁止的 Direct

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | PROXY_UNREACHABLE→显式错误 |
| F-02 | search provider 失败→按 profile fallback，不自动云模型 native search |
| F-03 | DNS rebinding→阻断 |
| F-04 | 远程 path 切换→session identity 保持 |
| F-05 | 同步 cursor 冲突→基于 revision/causation 合并 |

## H. 指标目录

search_latency_ms；proxy_fail_total；ssrf_block_total；remote_rtt_ms；relay_ratio；sync_conflict_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | HTTP_PROXY/HTTPS_PROXY 生效 |
| T-002 | PAC 不同 URL 选择不同路径 |
| T-003 | 302 到私网被阻断 |
| T-004 | Local DeepSeek + KYNXA Search 工作 |
| T-005 | Cloud fallback=DENY 时搜索失败不外传 query |

## J. 实施顺序
1. 先 NetworkProfile+Search/Fetch
2. 再 proxy/PAC
3. 最后 remote overlay/realtime sync

## K. 网络出口和 SSRF 的执行位置〔建议〕

NetworkProfile 分别描述 model egress、search/fetch、download 和 remote control，不以一个“联网”开关隐式授权所有流量。UI 开启搜索只表达用户意图；实际目标、scope 和外发内容仍由策略判断。Provider-native search 的网络发生在服务端，不能宣称本机代理控制了其全部路径。

代理解析先得出可解释 ProxyPlan，再执行连接。显式代理不可达时返回错误，除非该 Profile 事先允许 Direct fallback。PAC 是代码，需在受限环境求值并限制时间/网络；代理认证通过 CredentialRef 注入，NO_PROXY 不得绕过 Authority 的目标分类。

Web Fetch 对 URL 协议、主机、端口、每次 DNS 解析和每一跳 redirect 检查；阻止 loopback、私网、链路本地、保留地址及其 IPv4-mapped IPv6 等等价表示。连接必须使用已验证的目标并保持正确 TLS hostname 检验；重新解析或代理侧解析若无法验证最终目的地，应交给可信出口执行器或拒绝，不能只检查输入域名。

跨主机 redirect 不转发 Authorization/Cookie 等凭证；限制跳数、响应体大小、解压后大小和时间。检验 Content-Type 不替代内容校验。远程控制的已认证私网路径不自动成为 Web Fetch 的私网例外，两者权限分别建模。


# 17 · 数据库、本地持久化、关系模型与索引详细设计

**版本：v1.4.2-R3 · Full Technical Edition**

## 文档目的
本文件恢复为独立的可编码详细设计，不再作为短附录存在。

### 核心表关系建议
`work(parent_work_id)` 形成单父树；`conversation.work_id` 可空；`task.work_id` 必填，`task.conversation_id` 可空；`task_node.task_id` N:1；`cognitive_state.task_id` 维护当前 revision 并可有历史快照；`checkpoint` 保存恢复锚点；`session_binding` 把 task_node 与 DSH session/generation 关联；`artifact` 与 `file_blob` 分离，支持内容去重与多引用。

### 数据库所有权
Desktop 不直接访问数据库；Host 通过 Repository 层访问 `kynxa.db`；Authority 独占 `authority.db`；Agent Runtime 只通过 Host contract 获取必要状态。向量/FTS/DSH query index 均可删除重建，不能成为唯一来源。

## 模块工程要求

本章对象、服务、指标和测试同时适用 00.K 的通用契约；以下保留模块特有内容。

## A. 模块责任、边界与非目标

### A.1 本模块拥有的责任
| 责任 ID | 工程责任 |
|---|---|
| OWN-01 | kynxa.db 业务真相、authority.db 安全真相、文件系统真实字节、可重建索引的四层数据所有权 |
| OWN-02 | SQLite schema、FK、revision、transaction、migration、索引与归档 |
| OWN-03 | Work/Conversation/Task/CognitiveState/Knowledge/Artifact/Model/Skill/Remote 的关系映射 |
| OWN-04 | 引用优先文件策略与 blob/file dedup |
| OWN-05 | Qdrant/FTS/session query 等 derived index 的重建 |

### A.2 明确不拥有
- Authority secret 明文
- 把向量库当业务主库
- 把 Markdown 导出当数据库

## B. 核心对象目录

work；conversation；message；task；task_node；cognitive_state；checkpoint；artifact；file_blob；knowledge_source；document_version；chunk；skill；model_provider；model；session_binding；event_outbox；remote_device。

## C. 服务职责目录

DatabaseMigrator；Repository Layer；TransactionCoordinator；BlobStore；IndexRebuilder；RetentionManager。

## D. 主流程与状态推进
1. 业务写入在 SQLite transaction 内提交 truth row + outbox
2. 文件先 hash/canonical metadata，再建立 file_blob 引用
3. 索引 worker 消费 outbox 异步更新 FTS/vector
4. authority.db 只由 Rust Authority 拥有与迁移
5. 恢复时先 SQLite/files，再重建所有 derived index

## E. 不变量与安全要求
- 任何表都显式带稳定 ID 与必要的 scope/revision
- 跨 Work 查询必须携带 work_id/scope filter
- 重要写操作不跨数据库做“假原子事务”，而用 outbox/reconciliation
- Credential secret 不进入 kynxa.db
- Session JSONL/DSH storage 不替代 Task/CognitiveState 业务真相

## G. 错误、恢复与降级
| Failure ID | 处理 |
|---|---|
| F-01 | SQLite busy→有限重试+短事务 |
| F-02 | migration fail→备份并进入只读恢复 |
| F-03 | blob missing→标 broken ref 并尝试从源恢复 |
| F-04 | index out-of-sync→全量 rebuild |
| F-05 | outbox stuck→监控并重放幂等 consumer |

## H. 指标目录

db_write_latency_ms；sqlite_busy_total；migration_duration_ms；index_lag_seconds；blob_dedup_ratio；orphan_ref_total。

## I. 验收与回归测试
| Test ID | 场景/期望 |
|---|---|
| T-001 | 同一附件多 Conversation 引用只存一份 blob |
| T-002 | 删除向量索引后全量重建 |
| T-003 | Task/Checkpoint 与 DSH session binding 一致恢复 |
| T-004 | Child Work scope 查询隔离 |
| T-005 | 迁移前后 row count/hash/foreign key 一致 |

## J. 实施顺序
1. 先冻结 schema v1 + migration harness
2. 再接 file/blob/index outbox
3. 最后做 backup/restore/integrity sweeps

## K. 第一版关系模型与事务边界〔建议〕

### K.1 最小表集合

下表列出首个 CodeRepair 切片所需字段，不是可直接复制的完整迁移 SQL。类型、约束、级联行为与索引必须在 schema v1 评审后生成 migration；后续产品表按启用模块增量加入。可变实体另带 created_at、updated_at 和必要 revision；不可变版本及事件保留创建时间与来源引用。

| 表 | 最小关键字段 | 必要约束/索引 |
|---|---|---|
| work | id, parent_id, status, revision, deleted_at | parent FK；id≠parent；树循环在事务内检查 |
| conversation | id, work_id nullable, revision | work FK；索引(work_id, updated_at, id) |
| message | id, conversation_id, operation_id, state | conversation FK；operation_id 唯一 |
| task | id, work_id, conversation_id nullable, graph_revision, status | Work 必填；关联 Conversation 必须同 scope |
| task_node | id, task_id, kind, status, revision, active_execution_id | 索引(task_id,status)；复合唯一(task_id,id) |
| task_edge | task_id, from_node_id, to_node_id | 两端复合 FK；禁止自环；写入时检测 DAG |
| node_execution | id, node_id, attempt_no, fencing_token, lease, state | 唯一(node_id,attempt_no)；一个活动执行 |
| cognitive_state | task_id, revision, payload_ref, last_event_id | task_id 主键；条件更新 revision |
| session_binding | id, node_id, generation, session_ref, state | 唯一(node_id,generation)；一个活动绑定 |
| checkpoint | id, task_id, graph_revision, state_revision, manifest_ref | task FK；不可变版本记录 |
| artifact | id, work_id, task_id, current_version_id | Work/scope 一致；不以文件名作 ID |
| artifact_version | id, artifact_id, version_no, blob_ref, hash, provenance_ref | 唯一(artifact_id,version_no) |
| file_blob | id, hash, size, storage_ref, state | 去重键含存储/加密域；授权独立 |
| event_log | event_id, aggregate_id, sequence, payload_ref | 唯一(aggregate_id,sequence) |
| event_outbox | event_id, target, state, attempts, available_at | 唯一(event_id,target)；待投递索引 |
| consumer_inbox | consumer_id, event_id, processed_at | 复合主键；消费效果与去重同事务 |
| operation_result | subject_id, operation_id, request_hash, result_ref | 复合唯一；同 ID 不同 hash 拒绝 |

### K.2 外键不能代替 Scope 检查

task.conversation_id 存在只说明引用合法，不证明 Conversation 属于该 Work。建议用显式复合外键和非空的 scope discriminator，或 Repository 事务校验加数据库 trigger；不要依赖 nullable 复合 FK 自动检查所有组合。普通 Chat 升级为 Work 时采用显式绑定操作并记录事件，不能让 Task 偷偷访问 work_id=NULL 的任意对话。

task_edge 的两端必须属于同一个 task；session_binding 的 node 与 checkpoint 的 task 一致；Artifact 引用也做同 scope 校验。安全库里的 device/approval/grant 才是安全真相，业务库 remote_device 只允许存展示投影，不能独立编辑授权字段。

一个活动执行/绑定用部分唯一索引约束明确的活动状态集合，不能只靠 UI 或应用层先查后写。存在未决副作用的旧 execution 会阻止新执行获得相同资源写权限。外键删除默认 RESTRICT，归档、tombstone、审计保留和 GC 各自执行；不可对整棵 Work 树设置无差别级联物理删除。

### K.3 条件更新与事件提交示例

```sql
BEGIN IMMEDIATE;
UPDATE task_node
SET status = :next_status, revision = revision + 1
WHERE id = :node_id AND revision = :expected_revision
  AND status = :expected_status;
-- Repository 必须检查 affected_rows = 1，否则 ROLLBACK。
-- 然后在同一事务插入 event_log 与 event_outbox。
COMMIT;
```

以上片段是事务顺序示意，不能在未检查 affected_rows 时继续插入事件。每个 SQLite 连接显式启用外键；短事务不包含模型调用、网络请求或文件解析。BUSY 只有限重试，不能在 UI 线程阻塞。durability 参数由断电/崩溃要求冻结，不以默认配置推断已耐久。

### K.4 文件与数据库之间

Blob 先写同卷临时文件、校验大小与 hash、完成约定的持久化步骤，再切换到内容地址路径，最后提交引用。文件已落盘但事务失败会留下 orphan，交由宽限期 GC；引用先提交但文件未落盘会形成 broken ref，应从写入顺序上避免。多 scope 可共享字节，但各自持有引用和授权，知道 hash 不代表允许读取。

迁移记录 migration_id、checksum、applied_at 和 schema_version；启动先检测重复/不一致迁移，失败进入只读恢复。大索引回填与 schema 切换分阶段进行，期间查询能区分 not_ready。备份前后校验 FK、关键行数、引用可解析性和内容摘要，而非仅比较数据库文件大小。


# Appendix A · R2 → R3 完整迁移矩阵

本附录用于在不丢失 v1.4/v1.4.1/v1.4.2-R2 详细设计的前提下迁移到 DeepSeek Harness 内核。R3 的基本策略是“**保留产品与安全真相，替换重复的 Agent Runtime 基础设施**”。

| R2 组件/概念 | R3 归属 | 动作 | 说明 |
|---|---|---|---|
| Python central Orchestrator | KYNXA Host + DSH Runtime | 重构 | Python 降为 specialized workers |
| Planner/Agent loop | DeepSeek Harness agent-loop | 替换 | 不再自研通用 ReAct loop |
| Tool registry | DSH tools + KYNXA Tool Bridge | 复用+适配 | dangerous tool 仍走 Authority |
| Skill loader | DSH skill + KYNXA Governance | 复用+保留治理 | SKILL.md/版本/信任继续属于 KYNXA |
| MCP client runtime | DSH/MCP seam | 复用 | 权限和 secret 不下放 |
| Subagent engine | DSH subagent | 复用 | 子 agent 同样受预算/Authority 约束 |
| Workflow engine | DSH workflow | 复用 | 复杂编排按需开启 |
| Work | KYNXA | 保留 | DSH Workspace 不能替代 |
| TaskGraph | KYNXA | 保留 | 作为 durable outer orchestration |
| CognitiveState | KYNXA | 保留 | 与模型/Session 解耦 |
| Context Compiler | KYNXA→DSH adapter | 保留并改接口 | 注入 system-prompt/context seam |
| Model Manager/Runtime | KYNXA | 保留 | 通过 DSH LLM seam 提供模型 |
| Knowledge/Memory | KYNXA | 保留 | 只把 evidence/context 交给 DSH |
| Authority | Rust KYNXA | 完整保留 | 绝不被 DSH approval/sandbox 替代 |
| kynxa.db | KYNXA | 保留 | 业务 Source of Truth |
| DSH Session JSONL | DSH Runtime | 新增/明确 | Agent execution history，不是业务 DB |
| DSH storage/workspace | DSH host-side | 可选 | 仅 runtime 辅助/项目分组 |
| Android/Resident/Network | KYNXA | 保留 | 与 Harness 正交 |

## 迁移顺序
1. 冻结 00 Contract 与 Authority API。
2. 建 `kynxa-agent-runtime`，先跑 dsh minimal/base composition。
3. 实现 Context/Model/Tool/Event adapters。
4. 选 CodeRepair Vertical Slice 双跑旧 runtime 与 DSH runtime。
5. 对比 Task success、tool trace、checkpoint/resume、副作用一次性语义。
6. 关闭旧通用 agent loop，只保留必要 specialized worker。
7. 清理旧 Python Orchestrator 隐式权限与重复 session/storage。

## 本仓库适用性与内容保留说明

迁移矩阵保留用于解释历史设计归属；实际仓库目前只有 Desktop 原型，后端按 R3 新建。章节 00–17 和附录 A–E 的独特冻结结论、流程、失败模式、指标名称与验收场景均保留。重复的对象通用要求、组件通用约束、并发段落、观测原则和 Definition of Done 模板集中到 00.K。

新增章节 K 与附录增补均属于“工程建议”，不覆盖冻结要求。若出现冲突，按原规范优先级解决，并记录 ADR；不能以本版更晚为由直接把建议当成新冻结。原始压缩包保持不变，修改版以独立文件交付。


# Appendix B · DeepSeek Harness Upstream 映射与同步策略

基线日期：2026-09-18。DeepSeek Harness 官方仓库当前把 `packages/core` 作为 agent 主干：`scope / session / system-prompt / tools / agent / agent-default-model / agent-loop`。`agent-loop` 负责创建/恢复 agent、模型请求、流式响应、tool dispatch 和 durable session history。`storage` 提供 host-side 非 Session 数据，`workspace` 提供目录/Session 分组且对模型不可见。

## KYNXA 映射
| Upstream seam | KYNXA 使用方式 | 禁止事项 |
|---|---|---|
| session | 记录 agent execution history | 不替代 Task/CognitiveState |
| system-prompt | 注入 ContextBundle section | 不把 secret 放入 prompt |
| tools | 展示/分发 KYNXA Tool schema | 不直接执行危险 OS 动作 |
| agent/agent-loop | 通用 turn/step lifecycle | 不 fork 成 KYNXA 私有大分支 |
| LLM seam | 接 Model Resolver/Provider | 不让 DSH 保存 master key |
| skill | Skill runtime | 治理/权限仍在 KYNXA |
| subagent | child agent backend | child 不得越权 |
| workflow | complex orchestration | 仅 Deep Work/预算允许时启用 |
| web | search/fetch primitive | 网络策略/SSRF 仍由 KYNXA |
| storage | runtime sidecar state | 不存安全真相 |
| workspace | UI/host 项目分组可选 | 不替代 Work |

## Upstream 策略
- 锁定 commit/semver；生产构建保留 exact lockfile。
- KYNXA 默认只新增 package/profile/plugin/adapter；修改 upstream core 必须有独立 patch 文件、原因、覆盖测试与移除计划。
- 升级流水线自动运行 session format、resume、parallel tools、skill、subagent、workflow、web、Authority Bridge、Model Adapter、kill/restart 回归。
- upstream breaking change 先在 compatibility branch 验证，不直接落主线。

## 上游事实验证清单〔实施前必做〕

原文关于 DSH 的描述保留为 2026-09-18 文档基线，本次文档编辑没有联网验证其当前 API。必须在实施时填写 upstream URL、commit、license、实际包/导出名、Session 格式、工具 execute hook、取消语义、恢复语义和支持的平台。

兼容性 spike 至少产出：最小 Session 创建/恢复程序、一个被 Authority 拒绝的工具调用、流式取消测试、进程中断恢复测试和 lockfile。只要任一关键 seam 不存在，就记录差距和替代路径，不通过猜测 API 编写看似完整的集成代码。


# Appendix C · IPC Schema、数据对象与事件契约

## 通用 Envelope
```json
{
  "schema_version": "1",
  "message_id": "uuidv7",
  "correlation_id": "uuidv7",
  "causation_id": "uuidv7|null",
  "producer": "kynxa-host",
  "scope": {"work_id": "...", "task_id": "..."},
  "revision": 42,
  "type": "CreateSession",
  "payload": {}
}
```

## Host ↔ Agent Runtime
- CreateSession / ResumeSession / DeliverPrompt / InjectContext / CancelTurn / ReleaseSession / GetSessionProjection。
- SessionEvent / ModelCallEvent / ToolIntent / ToolResultEvent / SubagentEvent / WorkflowEvent / UsageMetric / FatalError。

## Host ↔ Authority
- EvaluateCapabilityRequest / CreateApprovalIntent / ClaimGrant / ResolveCredentialRef / CommitSideEffect / ReconcileSideEffect。
- Authority 的 response 必须有 decision、reason_code、grant_ref/approval_ref、expires_at、policy_revision。

## Host ↔ Model Runtime
- ResolveModelPlan / LoadModel / UnloadModel / GenerateStream / CancelGeneration / GetHardwareProfile。

## 事件原则
所有用户可见进度最终映射成稳定事件，而不是让 UI 解析日志字符串。事件 payload 有大小限制，大文件通过 Artifact/FileRef 引用。

## C.1 通用 Envelope 细化〔建议 v1〕

本附录原 JSON 是概要。本增补给出字段约束和消息族的最小 payload，供生成完整 JSON Schema/IDL；尚不是已经冻结或部署的 wire format。消息名是 KYNXA 逻辑操作，不映射猜测的上游函数名。

| 字段 | 类型/必需性 | 约束 |
|---|---|---|
| schema_version | string，必需 | 建议 1.0；协商支持的 major/minor |
| message_id | UUID string，必需 | 每次传输唯一，不作为业务幂等键 |
| operation_id | UUID string，命令必需 | 同一语义操作跨重试保持不变 |
| correlation_id | UUID string，必需 | 一次业务链路关联 |
| causation_id | UUID/null，必需 | 直接原因的 message/event ID |
| producer | string，必需 | 诊断声明；实际身份来自受信传输 |
| instance_id | UUID string，必需 | 进程实例，防旧实例消息混入 |
| type | enum，必需 | 白名单消息类型，决定 payload schema |
| scope | object，必需 | scope_kind + 对应 ID；GLOBAL/CONVERSATION/WORK |
| revision_refs | object，按消息要求 | object/graph/policy/scope revision，十进制字符串 |
| deadline_at | UTC string，命令必需 | 接收后检查；不作为唯一防重放手段 |
| payload | typed object，必需 | discriminator 对应固定 schema，拒绝未知字段 |

原 Envelope 的单 revision 不足以表达多对象版本依赖，建议迁移为 revision_refs，并用协议版本变更明确处理，不能让旧客户端默认读懂新字段。event timestamp 和 stream sequence 是事件族字段，不应被省略。

scope_kind=GLOBAL 用于明确的全局操作，不能表示任意 Work 通配符。TaskNode 命令要求 WORK scope、work_id/task_id/node_id 一致；普通 Chat 可使用 CONVERSATION scope。传入的 scope 是待验证声明，接收方根据身份和对象关系重新解析。

## C.2 消息族与必需 payload

| 消息 | 必需字段 | 成功结果/约束 |
|---|---|---|
| CreateSession | binding_id, node_id, generation, profile_ref, context_ref, mount_ref | session_ref；重复操作返回已有绑定 |
| ResumeSession | binding_id, session_ref, generation, checkpoint_ref, event_cursor | 当前投影或不可恢复原因 |
| DeliverPrompt | binding_id, turn_id, prompt_ref, context_revision | accepted_turn_id；同 turn 不重复执行 |
| InjectContext | binding_id, context_ref, expected_context_revision | 新 context revision；拒绝旧覆盖 |
| CancelTurn | binding_id, turn_id, reason | 接收确认；最终停止另发事件 |
| ReleaseSession | binding_id, expected_generation | 资源释放结果，不删除历史 |
| GetSessionProjection | binding_id, after_sequence | 投影、游标、缺口/快照指示 |
| ToolIntent | binding_id, execution_id, fencing_token, tool_call_id, tool/schema ref, args | observation 或待批准状态 |
| EvaluateCapabilityRequest | subject_ref, action, resource_claim, typed_params, revision_refs | DENIED / APPROVAL_REQUIRED / GRANT |
| ClaimGrant | grant_ref, execution_id, canonical_request_hash, executor_ref, fencing_token | claim_receipt；不得直接重复执行 |
| CommitSideEffect | execution_id, outcome, evidence_ref, result_hash | ledger revision；仅可信执行器提交 |
| ReconcileSideEffect | execution_id, evidence_cursor | 当前状态与允许的核实动作 |
| ResolveCredentialRef | credential_ref, purpose, destination, execution_id | opaque use_handle；不返回 raw Secret |
| ResolveModelPlan | requirements, mode, selected_ref, policy_refs, budget | 不可变 plan_ref 与 reason_code |

审批应补充 ResolveApprovalIntent：intent_id、decision、canonical_digest、expected_revision、受信用户/设备证明。已有 CreateApprovalIntent 只能在 Authority 已规范化请求后创建；普通客户端不得直接构造可信审批卡。LoadModel/UnloadModel/GenerateStream/CancelGeneration/GetHardwareProfile 仍保留，参数由独立 Model Runtime contract 冻结。

## C.3 帧、握手、事件流和背压

建议本地 Named Pipe 采用 4 字节无符号长度前缀 + UTF-8 JSON。默认单帧上限建议 1 MiB，可经双方握手选择更小值；大文件仅传 FileRef/ArtifactRef。限制 JSON 深度、字符串长度和解压后大小；长度异常立即拒绝，不能先按攻击者提供的大小分配内存。

握手协商协议版本、进程角色、instance_id、nonce、能力集合和最大帧；Windows 端点 ACL、对端 token 身份和 Authority 的角色授权共同生效。PID、producer 字符串和随机管道名都不能单独证明可信身份。具体 ACL/隔离实现通过 09.K 的 spike 冻结。

持久事件具有 event_id、stream_id、sequence、occurred_at、scope 与 payload。ACK 仅在本地持久提交后返回；重复 event_id 不重复 reducer。检测到序列缺口先补拉，超过保留窗口时拉 snapshot + 新游标。UI token 流可合并，不要求每 token 持久化；最终消息与工具/审批事件必须耐久。

Stop、Approval、TerminalEvent 使用独立高优先队列，不能与图片/Artifact 二进制共用无界缓冲。订阅者太慢时限制缓冲并要求重同步，不能丢失批准与终态而不告知。Cancel ACK 只代表收到命令，Cancelled/Interrupted 事件才说明生成已停止；副作用终止另按 ledger 判断。

## C.4 一次请求的错误响应形态

```json
{
  "operation_id": "<UUID>",
  "status": "error",
  "error": {
    "code": "SIDE_EFFECT_UNKNOWN",
    "category": "UnknownSideEffect",
    "safe_message": "操作结果尚未确认，正在核实。",
    "retry_advice": "reconcile_only",
    "diagnostic_ref": "<opaque-reference>"
  }
}
```

此为 payload 示例，实际仍包在 Envelope 内。幂等存储同时保存 request_hash 与结果引用；同 operation_id 不同请求返回 IDEMPOTENCY_CONFLICT。幂等记录保留期必须覆盖重试/恢复窗口，过期也不能自动把历史高风险操作当全新动作执行。


# Appendix D · 验收、测试与 Benchmark 矩阵

| 类别 | P0 场景 | 通过条件 |
|---|---|---|
| Chat/UI | 本地 Chat 流式 | 不阻塞 UI；中断可恢复/重试 |
| Work | Child Work / scope | 无循环；不跨 Work 泄漏 |
| DSH | kill/resume | Session/TaskNode 一致恢复 |
| Tool | file/code/browser | schema/timeout/verifier 完整 |
| Authority | request binding | 参数/revision 改变使批准失效 |
| Side Effect | UNKNOWN | 只 reconcile，不盲重试 |
| Credential | use-by-reference | 模型/日志拿不到 raw secret |
| Model | Smart/Prefer/Strict | 路由遵从 policy |
| Local Runtime | OOM | 受控 fallback 或明确失败 |
| Knowledge | scope + citation | 可回原始 source/version |
| Storage | index loss | 可从 truth store 重建 |
| Remote | multi-device | role/approval once 语义正确 |
| Network | SSRF/proxy | 私网阻断；不偷改网络路径 |
| Security | prompt injection | 无越权 side effect |
| Recovery | Host/Desktop crash | task/checkpoint/artifact 可恢复 |

## Benchmark 结果规范
每次公开结果必须记录：代码 commit、DSH upstream commit、模型/版本、provider、硬件、OS、数据集版本、seed、超时、budget、成功定义、样本数与失败分类。不得只给最佳单次。

## D.1 可执行验收卡〔建议〕

| Case ID | 操作/故障注入 | 通过证据 |
|---|---|---|
| CR-01 | 固定失败仓库完成修复 | baseline 失败，最终 required tests 全通过；diff/hash 可追溯 |
| AUTH-01 | 批准后修改文件路径或补丁 | 旧 digest 被拒绝；目标字节未变化 |
| AUTH-02 | 两设备同时批准并 claim | 一次新执行；另一请求只返回已有状态或冲突 |
| AUTH-03 | claim 后杀 Runtime | 旧 Grant 不再签发新执行权；按 ledger 核实 |
| AUTH-04 | 模拟 Host/Node 被攻陷直接访问 | 受保护库、Credential、未授权资源实际访问失败 |
| REC-01 | 写文件成功、回执返回前杀 Executor | hash/journal 核实后补投结果；没有重复应用 |
| REC-02 | Host 提交 Observation 后杀 Runtime | 恢复 Session 收到同 observation_id 一次语义效果 |
| REC-03 | 删除 Session 保留 checkpoint | 新 generation 恢复；保留 lineage 与未决副作用 |
| DATA-01 | 两并发 Work 移动形成潜在环 | 最多一个合法提交；最终无环 |
| DATA-02 | Work A 请求 Work B 的 Artifact | 即使知道 hash 也拒绝，日志可追踪 |
| NET-01 | 公网 URL 重定向私网/重绑定 | 每跳/连接目标复核并拒绝 |
| MODEL-01 | Strict 文本模型收到视觉任务 | capability unavailable；无隐式云调用 |
| MODEL-02 | 私有 Work 的本地模型 OOM | 原 policy 内降级或失败；禁止云外发 |
| UI-01 | 快速切 Work，旧事件迟到 | 不串 scope；焦点和选择状态正确 |
| UI-02 | 点击连接测试后修改 endpoint | 原结果过期；不得显示新 endpoint 已通过 |
| STORE-01 | 清除全部可重建索引 | 业务/安全事实保留，索引可重新生成 |
| CANCEL-01 | 外部请求已发出时 Stop | 不再调度新动作；在途结果显示核实中 |
| UP-01 | 升级 DSH 并读取旧 Session fixture | 契约/恢复通过或升级被阻断 |

每张验收卡还需保存 fixture hash、commit、环境、fault injection hook、步骤、超时、预期数据库状态、外部效果计数和日志引用。断言外部副作用不能只检查应用日志，应查看隔离测试资源或模拟服务的真实记录。安全测试使用合成凭证和隔离目录。

### D.2 必须形成 ADR 的待决项

| ADR | 待决问题 | 冻结所需证据 |
|---|---|---|
| ADR-001 | Windows 隔离身份、Authority 资源 ACL | 真实绕过测试与部署/升级可行性 |
| ADR-002 | DSH 上游 commit 与扩展接口 | 可运行 spike、license、Session 兼容结果 |
| ADR-003 | CanonicalAction 编码及路径身份 | 三语言黄金向量与路径逃逸测试 |
| ADR-004 | IPC 版本、帧大小、事件保留与游标 | 压测、断线恢复、旧版本拒绝用例 |
| ADR-005 | SQLite schema v1 与耐久配置 | 迁移、断电/崩溃、scope/FK 测试 |
| ADR-006 | Grant TTL、lease、取消期限与预算 | 延迟分布、并发与时钟变化测试 |
| ADR-007 | 首个 Provider/本地运行后端 | 能力探测、凭证、代理与取消验证 |
| ADR-008 | 首版补丁文件类型与隔离构建方式 | 用户文件保护、恢复与进程逃逸测试 |

这些待决项不能在实现中被静默填成“默认允许”。建议参数用于启动验证，不作为测量结果或发布承诺。相关高风险功能只有在所依赖 ADR 通过后才能启用。


# Appendix E · WinUI 3 · 1440×900 布局与组件基准

本附录固化 GUI 讨论中已经确认的工程原则，避免后续实现把视觉稿当成固定像素截图。

## 基准窗口
- 设计基准：1440×900 logical px。
- 主窗口支持缩放；所有主要区域用 Grid + Splitter/Resizable Pane，不写死绝对坐标。
- 左侧导航、顶部栏、对话主区、可选预览区均有 min/max；用户拖动后的比例本地持久化。
- 顶部栏需为后续预览、模式、模型、任务状态等入口保留扩展位，不把第一版控件写死。

## 左侧导航
- Logo、收起侧边栏、搜索、Chat/Work/Library/Knowledge/Models/Skills 等主项。
- 最近对话采用滚动列表；条目图标和行高偏紧凑，不能因条目增加让整个侧栏失衡。
- 底部设置入口固定可达。

## Chat/Work
- Chat 输入框视觉与普通 Chat、Work Conversation 统一，避免进入 Work 后重新发明一套消息组件。
- Work 是由多个 Conversation/Task/Knowledge/Artifact 组成的长期范围，左栏在 Work 内展示该 Work 的对话。
- 右侧预览仅在需要时打开，关闭后主内容区自动扩展。

## 图标与资源
- Logo/主要图标优先 SVG；WinUI 使用 PathIcon/SvgImageSource 等适配。
- 不在 XAML 中复制复杂路径多份；建立 ResourceDictionary。
- 图标尺寸、stroke、corner radius、spacing 全部 token 化，后续可统一调整。

## E.1 现有界面的实现差距

现有主窗口使用 1440×900 初始尺寸、资源字典、SVG 图标和 PNG 品牌图；这些资产保持既有视觉方向，不因架构细化重新绘制。当前布局持久化是 best-effort UI 偏好；Preview 只有预留字段/列，不能计为已完成预览能力。

模型管理已有三类页签与自定义连接表单，但模型状态、下载进度和连接诊断属于静态示例。实施验收应以 Host 返回的数据和真实操作为准；“已连接”“已加载”“824 ms”之类预设文字不可留在生产状态展示里。

本文保留原图标/Logo 方向：组件 SVG、品牌 PNG。原附录的“Logo 优先 SVG”解释为一般建议，不覆盖当前已经批准的 PNG 品牌资源；无需为形式统一重画品牌图。
