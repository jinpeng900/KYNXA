# KYNXA 模型网关

KYNXA 自有的本机模型 API 服务，使用 Node.js 内置 HTTP、fetch 和文件接口直接连接云端或本地服务。支持 OpenAI Chat Completions、OpenAI Responses 和 Claude Messages 三种协议；协议适配集中在 `protocols.mjs`，无需额外 SDK、外部源码目录或 npm 依赖。

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

- `GET /health`：健康状态，`conversationProtocol:1` 表示统一会话存储协议。旧网关应在无生成任务时退出后升级。
- `GET /api/conversations/catalog`：正式目录与聊天消息，字段为 PascalCase。
- `PUT /api/conversations/catalog`：`{Revision,Projects?,Chats?}`，仅替换指定范围的元信息并添加新用户消息；忽略前端 assistant 快照。版本过期返回 409。
- `GET /api/models`：已配置的连接与模型 ID，不返回密钥。
- `POST /api/models`：保存 `{providerId,displayName,baseUrl,apiKey?,models:[id,...],protocol?}`。协议可为 `openai-completions`（旧配置默认）、`openai-responses`、`anthropic-messages`。同一 ID 更新连接；密钥留空且地址不变时保留原密钥，地址改变时不会转移旧密钥。
- `POST /api/models/test`：同样的参数，读取服务的 `/models`，不保存。部分服务不提供此接口，可直接填写模型 ID 后保存。
- `POST /api/chat`：`{conversationId,message,provider,model,permissionMode}`，按连接协议请求 `/chat/completions`、`/responses` 或 `/messages` 并返回完整文本。Claude 使用 `x-api-key` 与版本头；Responses 使用客户端会话记录并设置 `store:false`。
- `POST /api/chat/stream`：相同参数，可额外传入 UUID 格式的 `requestId`（助手消息 ID）和 `userMessageId`（已保存用户消息 ID），返回 SSE 流。`GET /health` 中的 `streamProtocol:1` 表示已支持此接口。

会话只按稳定聊天 ID 标识；服务商和模型是每次回复的属性，切换后继续使用同一历史。同一聊天的完整回复和流式请求共用队列。正式日志保存全部消息，模型请求目前选取最近 50 轮完整问答，失败/中断尝试保留展示但不加入成功上下文。思考内容不会自动作为正文发送给其他模型。当前支持文本回复，不执行工具或文件操作；权限选择仅作为 UI 元数据，不授予模型系统权限。

## 统一会话存储

标准 Data 目录结构：

```text
Data/
  settings.json                               Storage.LayoutVersion、StoreId；保留其他用户设置
  catalog.json                                项目、聊天标题/排序/归档及删除标记；不含正文
  Projects/<projectId>/project.json            项目元信息清单，由 catalog.json 生成
  Projects/<projectId>/Memory/                 项目长期记忆预留目录
  Projects/<projectId>/Sessions/<chatId>/events.jsonl
  Projects/<projectId>/Sessions/<chatId>/attachments/
  Chats/<chatId>/events.jsonl                   普通聊天
  Chats/<chatId>/attachments/
  Memory/                                     用户长期记忆预留目录
  Index/search.sqlite                         可重建的项目/聊天元数据索引
  Trash/<chatId>/events.jsonl                   已删除记录，供撤销恢复
  Backups/conversations-v1/                    一次性迁移的原始备份
  Desktop/                                    布局、模型选择、内置工作文件夹
  Models/connections.json                     模型连接
```

工作文件夹仍由用户关联；不会在其中自动写聊天日志。`events.jsonl` 逐条追加带版本的消息事件，以稳定消息 ID 合成当前消息状态。流式检查点由后端每约 1.5 秒有增量时写入，完成/失败/取消时持久化最终状态；重启将未完成状态标记为中断。目录更新有版本检查和可恢复事务，尾部不完整事件会留存诊断副本后恢复，中间损坏会停止读取。

`data-layout.mjs` 统一负责首次初始化、已有目录补齐和版本检查。启动和项目目录变更时，自动建立上述目录；无对话的草稿仍不创建持久聊天。`project.json` 是由正式目录生成的可重建清单，项目名、排序和关联路径在软件中修改后同步更新，不作为第二份可独立写入的元信息来源。项目改名或重新关联工作文件夹不改变项目/聊天 ID，也不移动聊天记录。

`settings.json` 的 `Storage.LayoutVersion` 标记目录版本；缺少该文件的旧目录自动升级，保留用户已有的其他设置及数据。遇到未知版本或损坏配置时拒绝写入。`conversation-index.mjs` 生成真实 SQLite 列表/标题元数据索引；索引缺失时重建，损坏时保留副本再重建，重建后关闭数据库句柄以便迁移。搜索 UI、正文全文检索、长期记忆提取和自动摘要尚未实现，Memory 目前只预留目录。

删除将聊天目录（含附件）移到 `Trash` 并建立删除标记，后续模型请求不能重建同一聊天；撤销恢复原日志与附件。当前回收记录与迁移备份不会自动清理，也不会进入模型上下文。

首次访问会话接口时先验证旧 `Desktop/projects.json`、`chats.json`，备份它们及旧 `Models/sessions`，再迁移前端完整历史；原文件保留。旧模型日志按模型分片且没有可靠全局顺序，因此只作为备份保留，不猜测合并以免重复或混入已删除内容。迁移标记在成功后写入，重复启动不会重复导入。旧版无统一 Data 的 LocalState 路径由桌面启动环境 `KYNXA_LEGACY_DESKTOP_HOME` 提供；自定义模型目录非 `Models` 时会话放在其 `Conversations/`。

设置中的迁移和 `migrate-storage.mjs` 都会复制、校验正式会话、附件、记忆目录、用户设置、回收记录和备份，并同步内置工作文件夹路径。`initialize-storage.mjs` 在未启用的目标目录调用相同初始化逻辑，补齐结构并重建索引；只在全部成功后切换存储指针。初始化失败保留原指针与原数据。旧程序仍使用旧格式，迁移后不应交替运行旧版写入同一用户数据。

## 流式回复与思考内容

每个 SSE `data:` 为 JSON，固定带有 `type`、`conversationId`、`requestId` 和 `createdAt`。请求先返回 `started`；生成时分别发送 `text_delta` / `reasoning_delta`（`delta` 字符串）；结束时发送一次 `completed`（完整 `content`、`reasoning`），或 `interrupted` / `error`（已有 `content`、`reasoning` 和脱敏后的 `error`）。正式输出结束后用完整快照校准正文，避免把最终内容重复追加。请求参数错误在开始 SSE 前返回 JSON 错误；心跳为 SSE 注释。

上游使用 `stream:true`，收到内容立即转发。支持 Chat Completions 的 `content` / `reasoning_content` / `reasoning`，Responses 的正文和思考摘要事件，以及 Claude 的 `text_delta` / `thinking_delta`；不展示签名等不透明字段。若兼容服务忽略流式开关而返回 JSON，则按完整回复处理，不发起第二次模型调用。

思考区只显示模型接口实际返回的可见内容。OpenAI Responses 仅对官方地址的已知支持型号请求 `reasoning.summary:auto`，得到的是摘要；其他兼容地址不会强加此参数。Claude、本地服务是否返回思考取决于模型及服务端配置，当前不自动设置思考预算或解析正文中的 `<think>` 标签。协议可见字段参考 [OpenAI 推理文档](https://developers.openai.com/api/docs/guides/reasoning) 与 [Claude 流式文档](https://platform.claude.com/docs/en/build-with-claude/streaming)。

客户端断开或网关关闭会取消上游请求。默认连续 180 秒未收到上游数据则中断，整个生成最长 15 分钟；在 `ModelRuntime` 构造参数中分别通过 `idleTimeoutMs` 和 `streamTimeoutMs` 调整。网络断流、输出上限或工具调用结束均不算完整回复，也不会自动重试。重试应复用原 `requestId`：日志中已成功提交的同一请求直接重放结果，避免终止事件丢失后重复提交；同 ID 携带不同消息会被拒绝。成功回复的 ID、消息摘要和思考内容与会话原子保存，发送后续模型上下文时仅保留 `role` / `content`。

桌面预设包括 DeepSeek、Kimi、OpenAI、Anthropic / Claude、Google / Gemini、阿里云百炼 / Qwen、智谱 / GLM、MiniMax、xAI / Grok、本地 API（直接连接）、Ollama、LM Studio 和自定义服务。API Key 必须来自相应服务商；预设模型仍受账号额度、权限和服务可用性限制。模型列表支持搜索、勾选、模型 ID 和用途说明，获取列表后只启用所勾选的模型。当前桌面聊天只发送文本，即使模型本身还支持其他输入类型。

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
