# 工程细化增补源

本文件用于构建《R3 精炼与工程细化版》。各节是建议设计，不能单独视为冻结协议或已实现能力。章节标记由构建脚本读取。

<!-- CHAPTER:00 -->
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

<!-- CHAPTER:01 -->
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

<!-- CHAPTER:02 -->
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

<!-- CHAPTER:03 -->
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

<!-- CHAPTER:04 -->
## K. 知识入库与可撤销检索〔建议〕

Source 先保存来源与访问范围，再计算内容版本，解析工作在受限 Worker 运行。入库状态建议为 RECEIVED→STORED→PARSED→INDEXING→READY；解析失败保留原文件及错误引用，索引失败只影响检索，不删除原文件。用户能区分“文件已保存”和“已经可检索”。

Chunk 身份由 document_version_id、解析器版本和定位信息确定；embedding_model_id/dimension/index_generation 作为索引元数据，换模型不覆盖旧向量空间。旧引用保留到历史版本；物理文件只去重字节，不把不同 scope 的授权记录合并。

检索先做可访问集合过滤，再进行召回；返回前再次验证权限 revision，避免索引延迟导致越权。删除源时立即 tombstone 并从查询结果中过滤，随后异步清理向量、FTS 和缓存。即使索引删除失败，也不能继续把已删除源交给模型。

Memory/Decision/Experience proposal 需记录提出者、证据、scope、置信度和状态 CANDIDATE/ACCEPTED/REJECTED/SUPERSEDED。单次 Session 压缩结果只作为候选材料。冲突决策通过 supersedes 关联；没有足够证据时呈现冲突，不自动用最新一段模型文本覆盖事实。

<!-- CHAPTER:05 -->
## K. 能力包安装与运行闭环〔建议〕

Skill 生命周期建议为 DISCOVERED→QUARANTINED→VALIDATED→ENABLED；更新先安装到新版本目录，验证完成后切换活动版本引用。失败不污染旧版本。source commit、内容 hash、依赖 lock、许可证和声明能力纳入安装记录；增加能力后进入重新评估，而不是沿用旧信任标签。

Tool 注册保存 schema_hash、adapter_version、capability 映射和 observation 限制。execute 回调仅能进入受控桥接，禁止模型通过未登记的 alias 调到原始宿主 shell 工具。对所有注册工具做“执行入口到 Authority/隔离 Worker”的覆盖检查；只检查工具名称黑名单不够。

MCP 超时后的重试取决于工具效果类型，不能把普通 RPC 超时统一标为 Transient。第三方声明的 readOnly/idempotent 只作候选元数据，须由本地治理映射确认。Resource 文本和工具说明保留不可信来源标记；发现描述或 schema 变化时重新计算 hash 并阻断不兼容调用。

<!-- CHAPTER:06 -->
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

<!-- CHAPTER:07 -->
## K. 模型解析的确定性规则〔建议〕

### K.1 从策略到执行计划

Resolver 输入为 scope、required_capabilities、user_mode、selected_model_ref、privacy_policy_revision、network_profile_revision、资源快照和预算。先排除硬性禁止的 Provider、云外发和能力不满足者，再执行用户模式；候选排序必须稳定并有 reason_code。

Strict 只允许选定模型及其明确配置的运行参数变化；能力不足返回 CAPABILITY_UNAVAILABLE。Prefer Selected 优先选定模型，补充专门模型需满足已允许的候选集合与隐私政策。Smart 可以路由但不能越过硬约束。模式与快速/标准/深度的执行档位分开建模，不用一个 enum 同时表达两种语义。

ModelExecutionPlan 保存 plan_id、具体 model/version、provider/profile revision、实际参数、必要能力、fallback 列表、预算和失效条件。执行前再次检查策略 revision 和资源可用性。Fallback 需要重新生成计划并记录理由，不能修改既有计划后隐藏实际调用的模型。

### K.2 能力检测与资源准入

能力区分 declared、probed、unknown、unsupported，并记录检测用例和时间。Tool Calling 探测需验证参数可解析及返回值接续；HTTP 200 不代表支持完整协议。检测用合成输入，不发送用户 Work 内容；用户覆盖能力声明仍不能覆盖安全策略。

模型加载前预留 weights + KV + runtime overhead + safety margin 预算，实际数值由硬件测量确定，不能把示例 GPU 数字当保证。OOM 后释放损坏实例，按原政策降低 context/batch、offload 或换允许的模型；改变输入截断策略须显示信息损失，Strict 下换模型需用户调整选择。

取消模型流只停止后续生成，已计费 token 仍结算。重试发生在流已产生内容之后时，创建新的 generation 并明确替换/并列关系，禁止把两次回答无标记拼接。达到成本预算时停止新调用，并向用户报告已知用量与尚待对账部分。

<!-- CHAPTER:08 -->
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

<!-- CHAPTER:09 -->
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

<!-- CHAPTER:10 -->
## K. 远程命令、配对与撤销细化〔建议〕

PairingIntent 绑定 PC 公钥指纹、临时 nonce、有效期和单次消费标志。手机扫码后双方确认同一配对会话；二维码泄露不能长期提供控制权。设备长期身份保存公钥和撤销 revision，私钥留在设备安全存储，不把传输 peer ID 当授权身份。

RemoteCommand 携带 device_id、remote_session_id、request_id、sequence、payload_digest 和目标 scope。Host 对 device/session 校验后，以 request_id 去重；重复请求返回原 task_ref。相同 request_id 不同 digest 必须拒绝。sequence 防重放不替代请求去重，断线重连需显式协商游标。

撤销先提交 Authority 安全状态，再使后续请求失效；Host 缓存只可缩短有效期，不可延长已撤销设备权限。已发生的副作用不能因设备撤销而消失。屏幕流、事件流和控制命令采用独立队列；Stop/Pause 的延迟在大文件传输和弱网下单独测量。

<!-- CHAPTER:11 -->
## K. 金融边界的落地前置条件〔建议〕

当前切片仅保留商品比较和沙盒订单，不把真实支付接入视为 Agent 完成前提。启用真实支付前，必须确认 Provider 支持的幂等和订单查询语义、商家身份、币种小数位、认证方式及退款/撤销限制，逐项记录到 adapter compatibility matrix。

最终批准摘要覆盖商家标识、商品/数量、最终金额/币种、税费/运费、收货地址引用及订单版本；任一变化使批准失效。支付 token 绑定商家、金额和有效期；模型不处理可复用支付 Secret。Full Access 不能绕过 L4，自动化预算许可也不等于单笔支付证明。

支付超时后保持 UNKNOWN，按同一 provider operation key 查询。没有查询能力时停止并提示人工核实；不能以新 key 再次扣款。对外展示“请求已提交”“支付已确认”和“核实中”三个不同状态，验收包含重复 webhook、乱序回执、重复批准和重启后的核实。

<!-- CHAPTER:12 -->
## K. 调度发生项与执行去重〔建议〕

Routine 定义保存 schedule_version、timezone、missed_run_policy、quiet_hours、预算和权限 scope。每个 occurrence 由 routine_id、schedule_version 与计划 UTC 时间构成唯一键；DST 重复本地时间映射到不同 UTC instant，再按明确政策选择一次或两次。不能仅使用“日期+小时”去重。

在事务内写入 occurrence 和 Task/outbox；若已存在则返回已有 Run。睡眠恢复后先计算漏跑集合，再按 skip、run_once_latest 或 bounded_catch_up 策略处理。修改计划产生新版本，但需记录已承接的 occurrence，避免旧新版本交接重复。

建议首版同一 Routine 禁止重叠执行，前一次未结束时跳过或等待并记录原因。Pause 阻止新 occurrence，不自动撤销已经发出的副作用；Stop 当前 Run 复用 Task 取消/核实语义。连续失败达到阈值进入 Needs You，阈值作为显式配置，不写死成无限重试。

<!-- CHAPTER:13 -->
## K. 偏好治理与 Shadow 的可验证边界〔建议〕

偏好记录 value、scope、source、evidence_ref、confidence、revision、status 和可选 expires_at。显式用户设置优先于推断；Work 偏好只在对应 scope 注入。用户删除后立即停止新 ContextBundle 使用，异步清理派生缓存，并提示已发送到外部 Provider 的内容不能被本地删除操作撤回。

Shadow Mode 在执行网关层强制拒绝真实副作用，并用模拟 Observation 驱动后续步骤，不能仅依赖 Prompt 说“不要执行”。只读观察也需原有 scope 和网络策略。候选行为记录预期动作与可用证据，评估误报、接受和撤销率；单次接受不足以升级为自动执行权限。

Constitution 和偏好都只是上下文约束，不写入 Authority Policy。若用户明确要求改变权限，需要走独立的策略编辑与授权路径，并展示变化范围。

<!-- CHAPTER:14 -->
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

<!-- CHAPTER:15 -->
## K. 首个发布切片的证据要求〔建议〕

将完成度拆成 UI 原型、可运行闭环、故障恢复、安全隔离和性能证据五项，分别报告。现有桌面 UI 不计为模型接通或 Authority 完成；文档列出的测试也不计为已通过测试。

建议 P0 内部集至少覆盖：真实失败仓库修复、审批拒绝、修改批准参数、两个 worker 争抢、执行后回执丢失、Host/Runtime kill、Session 丢失、索引重建、Strict 能力不足、云外发禁止和路径逃逸。每个 case 固定输入 fixture、注入位置、预期状态、可观察证据与通过判据，详见附录 D。

对照实验首先保持模型、任务、预算和工具能力一致，再比较 DSH 基线与 KYNXA 增量。能力成功率和安全阻断/误阻断率分别报告；恢复实验同时检查重复副作用计数。小样本只报告探索性结果，不能推导普遍优势。

性能数值先建立实际硬件基线：冷启动、首 token、流式 UI 更新、Authority 判定、checkpoint 提交、恢复耗时和内存峰值。本文不虚构已达成的毫秒数、显存占用或百分比；性能预算由测量后写入 ADR 与 CI Gate。

<!-- CHAPTER:16 -->
## K. 网络出口和 SSRF 的执行位置〔建议〕

NetworkProfile 分别描述 model egress、search/fetch、download 和 remote control，不以一个“联网”开关隐式授权所有流量。UI 开启搜索只表达用户意图；实际目标、scope 和外发内容仍由策略判断。Provider-native search 的网络发生在服务端，不能宣称本机代理控制了其全部路径。

代理解析先得出可解释 ProxyPlan，再执行连接。显式代理不可达时返回错误，除非该 Profile 事先允许 Direct fallback。PAC 是代码，需在受限环境求值并限制时间/网络；代理认证通过 CredentialRef 注入，NO_PROXY 不得绕过 Authority 的目标分类。

Web Fetch 对 URL 协议、主机、端口、每次 DNS 解析和每一跳 redirect 检查；阻止 loopback、私网、链路本地、保留地址及其 IPv4-mapped IPv6 等等价表示。连接必须使用已验证的目标并保持正确 TLS hostname 检验；重新解析或代理侧解析若无法验证最终目的地，应交给可信出口执行器或拒绝，不能只检查输入域名。

跨主机 redirect 不转发 Authorization/Cookie 等凭证；限制跳数、响应体大小、解压后大小和时间。检验 Content-Type 不替代内容校验。远程控制的已认证私网路径不自动成为 Web Fetch 的私网例外，两者权限分别建模。

<!-- CHAPTER:17 -->
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

<!-- CHAPTER:Appendix_A -->
## 本仓库适用性与内容保留说明

迁移矩阵保留用于解释历史设计归属；实际仓库目前只有 Desktop 原型，后端按 R3 新建。章节 00–17 和附录 A–E 的独特冻结结论、流程、失败模式、指标名称与验收场景均保留。重复的对象通用要求、组件通用约束、并发段落、观测原则和 Definition of Done 模板集中到 00.K。

新增章节 K 与附录增补均属于“工程建议”，不覆盖冻结要求。若出现冲突，按原规范优先级解决，并记录 ADR；不能以本版更晚为由直接把建议当成新冻结。原始压缩包保持不变，修改版以独立文件交付。

<!-- CHAPTER:Appendix_B -->
## 上游事实验证清单〔实施前必做〕

原文关于 DSH 的描述保留为 2026-09-18 文档基线，本次文档编辑没有联网验证其当前 API。必须在实施时填写 upstream URL、commit、license、实际包/导出名、Session 格式、工具 execute hook、取消语义、恢复语义和支持的平台。

兼容性 spike 至少产出：最小 Session 创建/恢复程序、一个被 Authority 拒绝的工具调用、流式取消测试、进程中断恢复测试和 lockfile。只要任一关键 seam 不存在，就记录差距和替代路径，不通过猜测 API 编写看似完整的集成代码。

<!-- CHAPTER:Appendix_C -->
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

<!-- CHAPTER:Appendix_D -->
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

<!-- CHAPTER:Appendix_E -->
## E.1 现有界面的实现差距

现有主窗口使用 1440×900 初始尺寸、资源字典、SVG 图标和 PNG 品牌图；这些资产保持既有视觉方向，不因架构细化重新绘制。当前布局持久化是 best-effort UI 偏好；Preview 只有预留字段/列，不能计为已完成预览能力。

模型管理已有三类页签与自定义连接表单，但模型状态、下载进度和连接诊断属于静态示例。实施验收应以 Host 返回的数据和真实操作为准；“已连接”“已加载”“824 ms”之类预设文字不可留在生产状态展示里。

本文保留原图标/Logo 方向：组件 SVG、品牌 PNG。原附录的“Logo 优先 SVG”解释为一般建议，不覆盖当前已经批准的 PNG 品牌资源；无需为形式统一重画品牌图。
