# KYNXA 模型网关

KYNXA 自有的本机模型 API 服务，使用 Node.js 内置 HTTP、fetch 和文件接口直接连接云端或本地服务。支持 OpenAI Chat Completions、OpenAI Responses 和 Claude Messages 三种协议；协议适配集中在 `protocols.mjs`，无需额外 SDK、外部源码目录或 npm 依赖。

需要 Node.js 22.19+。在项目根目录运行：

```powershell
node apps/model-gateway/server.mjs
```

默认监听 `127.0.0.1:5218`。`KYNXA_MODEL_API_PORT` 可修改端口，桌面端用 `KYNXA_MODEL_API_URL` 指定对应地址。

存储根目录通过 `~/.kynxa/storage.json` 的 `dataRoot` 指定，也可用 `KYNXA_DATA_HOME` 覆盖；桌面记录位于根目录的 `Desktop/`，模型连接和上下文位于 `Models/`。`KYNXA_MODEL_HOME` 可单独覆盖模型目录。未配置统一目录时，模型目录仍使用 `~/.kynxa/models`，桌面仍使用应用 LocalState，以兼容旧数据。

连接信息与密钥保存在模型目录的 `connections.json`，成功对话记录保存在 `sessions/`。文件写入采用临时文件和原子替换，API 响应不包含密钥。密钥目前为本机文件存储，并非加密保险库；该目录不应提交到版本控制。

当前开发机已迁移到 `D:\KYNXA\Data`。C 盘只保留存储位置指针和迁移前的原数据备份；应用后续聊天写入 D 盘。不要只修改指针来迁移已有数据。`migrate-storage.mjs` 在桌面和网关停止后，将两处旧数据复制到全新目录，逐文件校验 SHA-256、更新内置项目文件夹路径，最后才切换指针；原目录不会删除。外部关联的项目文件夹不改动。桌面布局设置首次启动时从旧 LocalSettings 导入 `Desktop/layout.json`。

接口：

- `GET /health`：健康状态。
- `GET /api/models`：已配置的连接与模型 ID，不返回密钥。
- `POST /api/models`：保存 `{providerId,displayName,baseUrl,apiKey?,models:[id,...],protocol?}`。协议可为 `openai-completions`（旧配置默认）、`openai-responses`、`anthropic-messages`。同一 ID 更新连接；密钥留空且地址不变时保留原密钥，地址改变时不会转移旧密钥。
- `POST /api/models/test`：同样的参数，读取服务的 `/models`，不保存。部分服务不提供此接口，可直接填写模型 ID 后保存。
- `POST /api/chat`：`{conversationId,message,provider,model,permissionMode}`，按连接协议请求 `/chat/completions`、`/responses` 或 `/messages` 并返回完整文本。Claude 使用 `x-api-key` 与版本头；Responses 使用客户端会话记录并设置 `store:false`。

会话按聊天 ID、连接 ID、模型 ID 隔离，保留最近 50 轮成功对话作为上下文，重启后仍可读取。同一会话的并发请求依次处理；失败请求不写入记录。切换模型时使用该模型对应的独立上下文。当前阶段支持纯文本完整回复，不执行工具或文件操作；权限选择仅作为 UI 元数据，不授予模型系统权限。

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

测试使用独立临时目录和本地模拟模型服务，覆盖真实 HTTP 调用链路、模型 ID 和密钥传递、多轮上下文、并发顺序、失败重试以及配置脱敏，无需云端密钥。
