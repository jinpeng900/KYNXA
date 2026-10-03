# KYNXA 模型网关

KYNXA 自有的本机模型 API 服务，使用 Node.js 内置 HTTP、fetch 和文件接口直接连接云端或本地服务。支持 OpenAI Chat Completions、OpenAI Responses 和 Claude Messages 三种协议；文本适配集中在 `protocols.mjs`，函数续接集中在 `tool-protocols.mjs`。MCP 使用官方 TypeScript SDK，技能 frontmatter 使用固定版本的 YAML 解析器；不依赖外部参考源码目录。依赖由 `npm ci` 或桌面构建恢复并复制到输出。

`server.mjs` 负责路由、校验、业务调用及网关关闭；`agent-http-routes.mjs` 集中工具设置与技能包接口；`http-transport.mjs` 负责 JSON 响应、按字节限额读取请求、SSE 帧和响应生命周期。传输模块管理心跳、背压和断开订阅，生成控制器集合、终止状态及持久化仍由服务端与 runtime 管理。职责提取保留既有端点、字段、状态码、请求限额及本机绑定；相关边界由网关和流式集成测试验证。

需要 Node.js 22.19+。桌面应用启动时会自动在后台启动本机网关，已有健康网关时直接复用；请求前也会检查并在网关退出后重新启动。网关脚本随桌面构建和发布复制，Node.js 可安装在系统中，或由发行包提供 `runtime/node.exe`。关闭桌面不会停止共享网关。显式配置的远程地址不会在本机自动启动服务。本地模型推理进程仍需单独启动。

开发调试也可以在项目根目录手动运行：

```powershell
node apps/model-gateway/server.mjs
```

默认监听 `127.0.0.1:5218`。`KYNXA_MODEL_API_PORT` 可修改端口，桌面端用 `KYNXA_MODEL_API_URL` 指定对应地址。

存储根目录通过 `~/.kynxa/storage.json` 的 `dataRoot` 指定，也可用 `KYNXA_DATA_HOME` 覆盖；桌面布局与偏好位于根目录的 `Desktop/`，模型连接位于 `Models/`，正式聊天记录由网关统一管理。`KYNXA_MODEL_HOME` 可单独覆盖模型目录。未配置统一目录时，模型目录仍使用 `~/.kynxa/models`，桌面仍使用应用 LocalState，以兼容旧数据。

连接信息与密钥保存在模型目录的 `connections.json`。会话存储由 `conversations.mjs` 管理，前端通过 HTTP 读取和更新目录，不再维护另一份聊天 JSON。模型 API 响应不包含密钥。密钥目前为本机文件存储，并非加密保险库；该目录不应提交到版本控制。

当前开发机已迁移到 `D:\KYNXA\Data`。C 盘只保留存储位置指针和迁移前的原数据备份；应用后续聊天写入 D 盘。不要只修改指针来迁移已有数据。`migrate-storage.mjs` 在桌面和网关停止后，将两处旧数据复制到全新目录，逐文件校验 SHA-256、更新内置项目文件夹路径，最后才切换指针；原目录不会删除。外部关联的项目文件夹不改动。桌面布局设置首次启动时从旧 LocalSettings 导入 `Desktop/layout.json`。

接口：

- `GET /health`：健康状态，`conversationProtocol:1` 表示统一会话存储，`contextProtocol:2` 表示独立输出预算、v2 可回源摘录和工具循环压缩，`agentProtocol:5` 表示远程连接、预设、认证引用、技能包、独立扩展存储与自动目录框架初始化，`toolStreamProtocol:3` 表示有序助手消息段、独立最终正文及完整工具结果合同。桌面仍接受旧 v2 消息。`extensionStorageProtocol:1`、`extensionRoot` 是扩展存储版本与当前实际目录。桌面不复用旧网关，应在无生成任务时退出旧进程后升级。
- `GET /api/conversations/catalog`：正式目录与聊天消息，字段为 PascalCase。
- `PUT /api/conversations/catalog`：`{Revision,Projects?,Chats?}`，仅替换指定范围的元信息并添加新用户消息；忽略前端 assistant 快照。版本过期返回 409。
- `GET /api/models`：已配置的连接与模型 ID，不返回密钥。
- `POST /api/models`：保存 `{providerId,displayName,baseUrl,apiKey?,models:[id,...],protocol?,contextWindowTokens?,maxOutputTokens?}`。协议可为 `openai-completions`（旧配置默认）、`openai-responses`、`anthropic-messages`。上下文默认 8192，可配置 2048–2000000；最大输出默认 262144（256K），可配置 1024–262144。两项独立，须匹配实际模型服务，小窗口会下调实际输出。旧客户端省略字段保留已保存值，旧连接未存输出字段使用新默认。同一 ID 更新连接；密钥留空且地址不变时保留原密钥，地址改变时不会转移旧密钥。
- `POST /api/models/test`：同样的参数，读取服务的 `/models`，不保存。部分服务不提供此接口，可直接填写模型 ID 后保存。
- `POST /api/chat`：`{conversationId,message,provider,model,permissionMode}`，按连接协议请求 `/chat/completions`、`/responses` 或 `/messages` 并返回完整文本。Claude 使用 `x-api-key` 与版本头；Responses 使用客户端会话记录并设置 `store:false`。
- `POST /api/chat/stream`：相同参数，可额外传入 UUID 格式的 `requestId`（助手消息 ID）和 `userMessageId`（已保存用户消息 ID），返回 SSE 流。`GET /health` 中的 `streamProtocol:1` 表示已支持此接口。
- `GET /api/agent/tools` / `POST /api/agent/mcp/refresh`：管理目录包含原始工具名与 enabled 状态；配置的 `disabledTools` 排除执行和模型声明中的对应项。
- `GET /api/agent/mcp/catalog` / `POST /api/agent/mcp/catalog/:id/add`：官方 Playwright、GitHub 预设；添加需 expectedRevision，默认禁用、重复复用。
- `POST /api/agent/mcp/disconnect` / `reconnect`：`{serverId}`，断开或明确重连，不重放上次执行；列表和连接响应含脱敏 `connections` 状态。
- `GET /api/agent/skills/:id/inspect|check|resource`：包清单、环境诊断、资源分页（resource 带 path/offset/limit）；`POST /api/agent/skills/import {directory}` 原子导入完整包。禁用项只在管理目录展示，模型工具不能绕过开关。
- `GET /api/conversations/:chatId/tool-results/:resultId`：公开完整结果；带 `offset` / `limit` 时返回最多 16000 字符的公开 JSON 页，详情见 [基础工具指南](../../docs/architecture/agent-tools.md)。

扩展存储由 `extension-storage.mjs` 解析，按扩展环境变量、独立用户指针、旧正式 Data 根依次选择。Agent 配置、导入 Skills、MCP npm/浏览器缓存可独立于聊天 Data；内置技能随应用发布，外部用户路径保持不变。设置迁移共用 Data 维护锁，复制校验和配置路径/禁用 ID 更新完成后才切换指针；无活动请求时先关闭原运行时，再串行重建，关闭失败禁止继续写入并显示安全错误码。位置更改的具体目录和界面见 [工具与技能存储](../../docs/architecture/agent-tools.md#使用入口与代码归属)。

会话只按稳定聊天 ID 标识；服务商和模型是每次回复的属性，切换后继续使用同一历史。同一聊天的完整回复和流式请求共用队列。正式日志保存全部消息，没有 200 条截断；`context.mjs` 按配置窗口选择近期完整问答、已确认记忆和当前请求相关的旧约束/代码摘录，预留输出与安全余量。`context-history.mjs` 的 v2 导航带真实消息来源，基础工具 `conversation.history.search/read` 可按需分页查回当前有效聊天公开正文，排除思考、配置、工具内部记录和兄弟聊天。工具循环按压力缩小已保存结果与旧历史的请求投影，保留调用/返回配对和原生续接字段。失败/中断尝试保留展示但不加入成功上下文。完整规则见 [聊天与工作记忆](../../docs/architecture/chat-work-memory.md#请求上下文与恢复) 与 [基础工具指南](../../docs/architecture/agent-tools.md)。

## 聊天与工作记忆

同一工作中的聊天各自保存历史，通过确认的工作记忆延续约定；其他聊天的完整正文不会自动混入当前上下文。普通聊天和「不使用文件夹」的工作聊天各自隔离。用户全局记忆只在明确指定时创建。完整规则及文件职责见 [聊天与工作记忆架构](../../docs/architecture/chat-work-memory.md)。

聊天消息以 `记住：内容` 或 `记住这个：内容` 开头时，在真实项目中保存为工作记忆，在普通/无文件夹聊天中保存为聊天记忆。`聊天记住：` 只影响当前聊天，`项目记住：` / `工作记住：` 明确共享到当前工作，`全局记住：` 保存用户级记忆。冒号可使用中文或英文；引用、代码和模型回复不会触发提取。

- `GET /api/conversations/:chatId/relationships`：当前工作归属、兄弟聊天 ID/标题/归档状态、可用记忆范围，不返回兄弟聊天正文。
- `GET /api/conversations/:chatId/memory`：按范围返回确认条目、版本、来源与有效状态。
- `POST /api/conversations/:chatId/memory`：`{scope,content,kind?,source?,expectedRevision?}`，scope 为 `chat` / `project` / `user`。
- `PATCH /api/conversations/:chatId/memory/:memoryId`：`{scope,expectedRevision,content?,kind?}`。
- `DELETE /api/conversations/:chatId/memory/:memoryId`：`{scope,expectedRevision}`。修改/删除必须提供范围文档当前版本，冲突返回 409。

设置中的记忆管理窗口已接入三个范围。聊天接口仍以正式聊天 ID 为入口；全局与工作有独立管理接口，不创建空聊天：

- `GET/POST /api/memory/user`；`PATCH/DELETE /api/memory/user/:memoryId`。
- `GET/POST /api/projects/:projectId/memory`；`PATCH/DELETE /api/projects/:projectId/memory/:memoryId`。

独立接口返回单个范围文档 `{schemaVersion,scope,scopeId,revision,entries,dismissedSources}`；写入参数和版本约定与上述聊天接口一致，若提供 `scope` 必须匹配 URL。仅创建用户手动确认的记忆，来源可没有聊天 ID；消息来源仍经聊天接口严格验证。真实归档工作可查看和编辑，归档工作记忆仍不注入上下文。隐藏无文件夹、未知、已删除工作拒绝访问。

`expectedRevision` 使用范围文档 `revision`（聊天 GET 为 `scopes[].revision`），不能使用条目版本。桌面每次提交已读版本，成功与 409 后重新 GET 来源状态；冲突保留编辑，不自动覆盖。新接口返回 `active/sourceAvailable/sourceArchived`，旧聊天写响应保持兼容。未发送草稿不会提前保存。当前没有范围批量清除接口。

记忆来源归档后仍可用；来源聊天删除后暂停注入，撤销删除后恢复。聊天移出原工作时，仅暂停从该聊天确认的原工作记忆；聊天记忆随会话移动，全局记忆继续按其明确范围生效。手动确认的记忆不依赖来源消息存活。删除明确记忆后，重试原请求不会将它重新创建。记忆文件损坏、未知版本、容量超限或不安全路径均拒绝覆盖，并返回明确错误。`GET /health` 通过 `memoryProtocol:1`、`contextProtocol:2` 标识记忆及上下文能力，`memoryManagementProtocol:1` 标识独立范围管理，桌面拒绝复用旧协议网关。

## 统一会话存储

标准 Data 目录结构：

```text
Data/
  settings.json                               Storage.LayoutVersion、StoreId；保留其他用户设置
  catalog.json                                项目、聊天标题/排序/归档及删除标记；不含正文
  Projects/<projectId>/project.json            项目元信息清单，由 catalog.json 生成
  Projects/<projectId>/Memory/entries.json     已确认的工作共享记忆
  Projects/<projectId>/Sessions/<chatId>/events.jsonl
  Projects/<projectId>/Sessions/<chatId>/context.json
  Projects/<projectId>/Sessions/<chatId>/Memory/entries.json
  Projects/<projectId>/Sessions/<chatId>/attachments/
  Projects/<projectId>/Sessions/<chatId>/tool-results/<resultId>.json
  Chats/<chatId>/events.jsonl                   普通聊天
  Chats/<chatId>/context.json                   可重建的本聊天摘录
  Chats/<chatId>/Memory/entries.json            已确认的聊天记忆
  Chats/<chatId>/attachments/
  Chats/<chatId>/tool-results/<resultId>.json     完整工具结果，按需创建
  Memory/entries.json                          已确认的用户全局记忆
  Index/search.sqlite                         可重建的项目/聊天元数据索引
  Trash/<chatId>/events.jsonl                   已删除记录，供撤销恢复
  Backups/conversations-v1/                    一次性迁移的原始备份
  Desktop/                                    布局、模型选择、内置工作文件夹
  Models/connections.json                     模型连接
  Agent/config.json                           MCP 服务/工具开关、技能目录与配置 revision
  Skills/                                     用户应用技能
```

工作文件夹仍由用户关联；不会在其中自动写聊天日志。`events.jsonl` 逐条追加带版本的消息事件，以稳定消息 ID 合成当前消息状态。流式检查点由后端每约 1.5 秒有增量时写入，完成/失败/取消时持久化最终状态；重启将未完成状态标记为中断。目录更新有版本检查和可恢复事务，尾部不完整事件会留存诊断副本后恢复，中间损坏会停止读取。

`data-layout.mjs` 统一负责首次初始化、已有目录补齐和版本检查。启动和项目目录变更时，自动建立上述目录；无对话的草稿仍不创建持久聊天。`project.json` 是由正式目录生成的可重建清单，项目名、排序和关联路径在软件中修改后同步更新，不作为第二份可独立写入的元信息来源。项目改名或重新关联工作文件夹不改变项目/聊天 ID，也不移动聊天记录。

`settings.json` 的 `Storage.LayoutVersion` 标记目录版本；缺少该文件的旧目录自动升级，保留用户已有的其他设置及数据。遇到未知版本或损坏配置时拒绝写入。记忆文件在首次确认记忆时创建；`context.json` 在历史超出本次输入预算时生成确定性的原文摘录，通过来源指纹核验并重建，不替代原日志，也不是模型生成的语义摘要。`conversation-index.mjs` 生成真实 SQLite 列表/标题元数据索引；索引缺失时重建，损坏时保留副本再重建，重建后关闭数据库句柄以便迁移。搜索 UI、正文全文检索、向量检索和模型自动提取记忆尚未实现。

删除将聊天目录（含附件、记忆和工具结果）移到 `Trash` 并建立删除标记，后续模型请求或结果保存不能重建同一聊天；撤销恢复整个目录。当前回收记录与迁移备份不会自动清理，也不会进入模型上下文。

首次访问会话接口时先验证旧 `Desktop/projects.json`、`chats.json`，备份它们及旧 `Models/sessions`，再迁移前端完整历史；原文件保留。旧模型日志按模型分片且没有可靠全局顺序，因此只作为备份保留，不猜测合并以免重复或混入已删除内容。迁移标记在成功后写入，重复启动不会重复导入。旧版无统一 Data 的 LocalState 路径由桌面启动环境 `KYNXA_LEGACY_DESKTOP_HOME` 提供；自定义模型目录非 `Models` 时会话放在其 `Conversations/`。

设置中的迁移和 `migrate-storage.mjs` 都会复制、校验正式会话、附件、记忆目录、用户设置、回收记录和备份，并同步内置工作文件夹路径。`initialize-storage.mjs` 在未启用的目标目录调用相同初始化逻辑，补齐结构并重建索引；只在全部成功后切换存储指针。初始化失败保留原指针与原数据。旧程序仍使用旧格式，迁移后不应交替运行旧版写入同一用户数据。

## 流式回复与思考内容

每个 SSE `data:` 为 JSON，固定带有 `type`、`conversationId`、`requestId` 和 `createdAt`。请求先返回 `started`；生成时分别发送 `text_delta` / `reasoning_delta`（`delta` 字符串）；结束时发送一次 `completed`（完整 `content`、`reasoning`），或 `interrupted` / `error`（已有 `content`、`reasoning` 和脱敏后的 `error`）。正式输出结束后用完整快照校准正文，避免把最终内容重复追加。请求参数错误在开始 SSE 前返回 JSON 错误；心跳为 SSE 注释。

工具流 v3 每轮先发送 `assistant_segment`，其中 `segment` 包含稳定 `id`、`round`、`order`、`phase`、`status`、公开 `content` / `reasoning` 和 `reasoningDurationMs`。该轮增量带 `segmentId`；模型轮结束后再次发送段快照，含工具调用的轮次为 `commentary`，循环真正结束的轮次为 `final_answer`。工具调用、审批和结果带同一 `round` / `order`。`content_snapshot` 保留累计文本兼容用途；`completed.content` 与正式消息 `Content` 只保留最终正文，全部阶段保存在 `AssistantSegments` / `completed.assistantSegments`。中断仍保存公开部分与已完成工具回执，旧消息不推测阶段边界。原生签名和加密推理仅留在供应商续接投影，不写入公开段落。

回复的总耗时由网关保存为 `DurationMs`，HTTP 回复和 SSE 末事件返回 `durationMs`，包含本次模型与工具执行，使用单调时钟测量，不计排队时间。已完成请求重放和历史读取沿用保存值，旧记录未知耗时为 0；只有已有可靠终态工具起止时间时才投影旧耗时。health 的 `replyTimingProtocol:1` 表明支持该可选字段。桌面成功完成后只显示最终正文和灰色用时，近期过程仅在运行时紧凑显示；隐藏过程不删除日志，不改变后续模型的配对历史。

工具选择支持中文和英文的查询意图：事实查询软优先已有多结果搜索、Fetch，代码文档软优先已有文档工具；不改变目录权限和每轮预算。一次模型回复中明确的独立读取最多四个并行，浏览器、写入、终端、技能与未知 MCP 顺序执行，所有调用仍经过原审批。每批全部结算并保存返回回执后才处理取消或失败。配置无变化的目录刷新复用连接；纯技能变更保留 MCP 进程但撤销旧请求，真实 MCP 配置变化保守重连。遇到 `AGENT_CONFIG_CHANGED` / `MCP_CATALOG_CHANGED` 保存失败回执后停止旧循环，不继续调用旧目录。

上游使用 `stream:true`，收到内容立即转发。支持 Chat Completions 的 `content` / `reasoning_content` / `reasoning`，Responses 的正文和思考摘要事件，以及 Claude 的 `text_delta` / `thinking_delta`；不展示签名等不透明字段。若兼容服务忽略流式开关而返回 JSON，则按完整回复处理，不发起第二次模型调用。

思考区只显示模型接口实际返回的可见内容。OpenAI Responses 仅对官方地址的已知支持型号请求 `reasoning.summary:auto`，得到的是摘要；其他兼容地址不会强加此参数。Claude、本地服务是否返回思考取决于模型及服务端配置，当前不自动设置思考预算或解析正文中的 `<think>` 标签。协议可见字段参考 [OpenAI 推理文档](https://developers.openai.com/api/docs/guides/reasoning) 与 [Claude 流式文档](https://platform.claude.com/docs/en/build-with-claude/streaming)。

客户端断开或网关关闭会取消上游请求。默认连续 180 秒未收到上游数据则中断，整个生成最长 30 分钟；在 `ModelRuntime` 构造参数中分别通过 `idleTimeoutMs` 和 `streamTimeoutMs` 调整。网络断流和输出截断不算完整回复；正常工具调用结束后循环继续，不要求用户再发「继续」。同一已完成 `requestId` 只重放结果，同 ID 携带不同消息拒绝。已有工具执行记录的失败请求不能直接重试重做；检查回执后发新消息。成功回复、公开阶段和执行回执与会话持久化。

后续模型输入由 `model-transcript.mjs` / `model-history.mjs` 从同一正式记录派生，保留公开助手调用与工具观察的整轮配对，而非只有最终 `Content`。先按预算缩减旧大结果，再缩减完整旧组，原始消息和档案不删除；超过 200 条消息仍可在同一聊天继续。失败草稿不当成功答案，已执行动作仍按真实状态提供观察。工具禁用或当前请求无工具目录时用明确的公开历史文本降级，不补旧工具授权或重做操作。公开接口不返回内部 `ModelTranscript`；必要原生续接只存在受保护结果档案 `_meta`，跨用户轮不沿用绑定旧前缀的签名。它不是完整任务检查点或跨崩溃自动续跑。

窗口和输出预算分别配置。单次输出应用上限仍为 262144；较大窗口给历史/代码保留更多空间，已核准的官方模型能力单独限制输入、输出和总窗口。新建已知云端连接按选中模型采用较大默认窗口，本地/未知模型保留 8192，已有用户值不自动改写。健康接口 `contextProtocol:3` 与 `toolStreamProtocol:3` 防止新版桌面复用缺少配对历史或阶段事件的旧网关。公式与参考来源见 [聊天上下文](../../docs/architecture/chat-work-memory.md#请求上下文与恢复)及 [工具历史](../../docs/architecture/agent-tools.md#跨消息的工具依据2026-10-04)。

桌面预设包括 DeepSeek、Kimi、OpenAI、Anthropic / Claude、Google / Gemini、阿里云百炼 / Qwen、智谱 / GLM、MiniMax、xAI / Grok、本地 API（直接连接）、Ollama、LM Studio 和自定义服务。API Key 必须来自相应服务商；预设模型仍受账号额度、权限和服务可用性限制。模型列表支持搜索、勾选、名称和模型 ID，获取列表后只启用所勾选的模型。当前桌面聊天只发送文本，即使模型本身还支持其他输入类型。

## 直接连接本地模型服务

在模型管理中选择「本地 API（直接连接）」，填写已经启动的服务地址，例如 `http://127.0.0.1:8080/v1`，或局域网服务 `http://192.168.1.25:8000/v1`。地址填写到 API 根路径，不要追加 `/chat/completions`。无认证的服务可将 API Key 留空；启用认证时填写该服务的密钥。点击「获取模型 / 测试」读取 `/models`，也可以手动填写服务所接受的模型 ID 后保存。

聊天直接向该地址的 `/chat/completions` 发送请求，不经过 Ollama。本机回环地址、IPv4 私网地址（10/8、172.16/12、192.168/16）和 IPv6 ULA 地址支持 HTTP 与可选密钥；局域网服务请填写 IP。公网服务继续要求 HTTPS 和密钥。

本入口连接兼容 OpenAI 的文本模型 HTTP 服务，不直接加载模型权重，也不自动启动推理进程。

### 启动已有 GGUF 权重

附带的 `start-local-model.ps1` 可以调用已安装的 llama.cpp `llama-server.exe`，隐藏启动服务并保存日志，不下载模型、不启动 Ollama 守护进程。例如：

```powershell
./apps/model-gateway/start-local-model.ps1 -ServerPath 'C:\tools\llama.cpp\llama-server.exe' -ModelPath 'D:\models\Qwen3-8B-Q4_K_M.gguf' -ModelId qwen3-8b
```

默认 `127.0.0.1:8080`、8192 上下文、单并发，参数可以覆盖。必须先启动本地服务，再启动 KYNXA 网关或桌面端；连接配置会保留，但推理进程不会在电脑重启后自动恢复。Ollama 路径也继续支持：开启 Ollama 服务后选择其预设，获取模型并保存，Qwen3 8B 通常使用 `qwen3:8b`，以该服务返回值为准。

启动脚本支持 `-BackendPath`（也可写入 `local-server.json` 的 `backendPath`），指定独立 GPU 后端 DLL。未指定时会探测引擎旁的 `cuda_v12`、`cuda_v13` 子目录，通过 `--list-devices` 验证可用后才加载。它会为子进程设置后端文件与依赖库搜索路径，启动后恢复脚本进程的环境变量。仅填写 `-ngl auto` 不代表已经启用显卡；应以启动日志和实际生成速度确认。当前开发机使用 CUDA 12 后端，CUDA 13 后端无法在当前驱动下初始化。

在当前开发机上已复用现有 Qwen3-8B GGUF 缓存与已有 llama.cpp 可执行文件，建立 `qwen3-local` 连接；机器专属路径未作为应用预设写死。可在模型管理中修改该连接地址。

该机器的启动路径保存在当前模型目录的 `local-server.json`（`serverPath`、`modelPath`、`modelId`），下次可直接执行 `./apps/model-gateway/start-local-model.ps1`。其他机器传入实际路径即可；`-ServerPath`、`-ModelPath` 显式参数优先。设置统一数据目录后，新启动进程的日志写入该目录下 `Logs/models`。

验证：

```powershell
node --test apps/model-gateway/tests/*.test.mjs
```

测试使用独立临时目录和本地模拟模型服务，覆盖真实 HTTP 调用链路、模型 ID 和密钥传递、多轮上下文、并发顺序、失败重试以及配置脱敏，无需云端密钥。流式测试还覆盖三种协议、分片 UTF-8 / SSE、思考与正文分离、最终快照、JSON 回退、取消与超时、半途出错、停止后的上下文和请求去重。

## 当前验证与后续接口

2026-10-02 实现基线 `3226527`：151 项网关自动测试通过；桌面构建零警告、零错误。记忆测试覆盖三层隔离、来源生命周期、版本冲突、迁移、摘要损坏恢复，以及记忆读写与会话移动/删除的并发顺序。该记录是此次基线结果，不是对未来提交、所有真实模型或 1M 推理能力的保证。

2026-10-03 工作区重构：HTTP 传输职责从路由中提取，完整网关回归 157 项通过、0 失败（原 151 项与新增 6 项）。新增检查覆盖 JSON 响应合同、请求体字节限额、分块 UTF-8、SSE 心跳、背压和订阅清理；正式日志、记忆范围和队列实现保持不变。

桌面记忆管理与独立最大输出配置已接入。新预算由 `output-budget.mjs` 统一计算，普通请求、流式及每轮工具请求一致；非流式截断也保留返回正文与可见思考，标记 interrupted，残缺工具参数不执行。`completed` SSE 事件的可选 `contextUsage` 字段返回估算用量、输出下调与摘录/工具压缩诊断，不新增未知事件类型；普通 HTTP 与桌面用量展示尚未接入。下一步把目录元信息与正文加载分开；模型的公开历史分页工具已实现，现有桌面 catalog 仍带全部消息。

基础文件工具、stdio / Streamable HTTP MCP、应用技能包与沙箱终端已接入；本地模型服务自动发现/启动与完整 Host 编排仍待实现。后续 Host 先复用网关的正式聊天/记忆服务，不增加第二套可独立写入的聊天历史。职责和验收见 [团队计划](../../docs/team/README.md)。

2026-10-03 上一阶段工具兼容与可靠性更新：发现目录和模型声明预算分离，支持工具启停、搜索和下一轮加载；原始 typed/structured MCP 结果按聊天保存，消息使用受限预览与完整引用；第三方参数与审批理由分离，包装保留原 schema 方言；技能支持安全 YAML 1.2；取消先保存已返回执行结果；长输出不破坏 JSON。工具续接复用普通聊天预算口径，避免重复计算正文的 JSON 转义。该阶段完整网关回归 239/239、桌面工具客户端 34 项、实际 DOM 20 项、独立原生窗口 78 项、语言 1751 项通过。

随后完成 MCP/Skill 能力补齐：使用官方 SDK 接入 Streamable HTTP、环境变量认证引用和 OAuth client credentials；支持目录通知、资源包装、断开/重连及自有 stdio 进程树清理。Playwright/GitHub 预设默认禁用、重复添加复用配置。技能支持标准包校验、资源分页、导入去重、启停、依赖诊断和 SHA-256 清单下的只读 Node 脚本沙箱；复用 Apache-2.0 沟通写作技能及现有浏览器、文件和记忆能力。完整网关 276/276、客户端 45、独立原生 UI 108、语言 1881 项通过；聊天流式、网关启动、模型上下文、正式聊天与 Data 迁移客户端检查通过。桌面编译零警告零错误，输出模块可独立导入，打包 ToolHost 的实际 AppContainer 技能执行通过。官方 Playwright MCP 实测发现 25 个工具并正常清理。Python/Bash/PowerShell 技能脚本与浏览器交互式 OAuth 尚不支持，也不保证每个模型都会正确选择工具。入口、文件归属及验证范围见 [工具与技能指南](../../docs/architecture/agent-tools.md)。

独立扩展存储补齐后的完整网关检查为 288/288；扩展迁移 73 项、原生存储 UI 116 项、语言 1968 项通过，桌面构建零警告零错误。路径解析、迁移锁、双根保密与客户端清理失败均有独立回归，实际新缓存目录中的 Playwright MCP 仍发现 25 个工具。

自动框架初始化与迁移完整性补齐后，网关全套 294/294、扩展迁移 96 项、简化后的原生 UI 129 项、语言 1994 项通过；Data 迁移及聊天/启动/上下文客户端检查保持通过，桌面构建零警告零错误。MCP、技能只保留两页，高级配置默认收起；首次使用、换目录及中断恢复规则见工具指南。版本 5 网关会自动初始化扩展框架，旧版网关不再被桌面复用。

2026-10-03 上下文与输出更新：网关完整回归 336/336，通过真实临时 JSONL 的 240→242 条消息重启/原文保留、260 条当前聊天历史检索、相关中段代码回源、三协议大结果压缩后的实际 HTTP 续接、取消回执、防重放及截断参数拒绝。原生模型管理窗口 53/53、语言 2019 项、模型配置/流式/正式会话客户端与网关启动检查通过（包括 contextProtocol:1 拒绝复用）；桌面构建零警告、零错误。均使用独立临时数据与模拟上游，未验收真实模型的大窗口效果。独立输出配置、上下文模块分工与边界见 [聊天与工作记忆](../../docs/architecture/chat-work-memory.md#请求上下文与恢复)。

2026-10-03 公开能力与执行更新：输出默认与本机现有连接按用户要求调整为256K；实际仍受上下文和供应商限制。目录新增11个经来源/许可证核对的MCP预设，标准技能完整包按固定commit/哈希导入；本机29个包中11个指令型已启用，7个MCP真实连接成功。连续工具调用默认64轮/256次/30分钟，生成预算与执行阶段单独保存；未实现后台子代理或崩溃自动续跑。网关349/349、原生工具展示114、窄窗灰色字体33、扩展迁移120及真实临时Python迁移通过。完整来源、运行条件和界面/存储边界见 [工具与技能指南](../../docs/architecture/agent-tools.md)。
