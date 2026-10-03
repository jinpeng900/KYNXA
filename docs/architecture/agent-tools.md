# 基础工具、MCP 与应用技能

本页描述已落地的第一版工具闭环。设置中的“工具与技能”打开独立窗口，管理 MCP 服务、查看应用技能和工具参数；配置按用户选择的 Data 目录保存。聊天输入区原有三种权限现在参与真实执行判定。

## 从模型请求到执行

桌面发送 `permissionMode`、聊天 ID 和请求 ID。Runtime 从正式 catalog 查找聊天所属工作和挂载目录，为这次请求固定范围与权限，提供函数目录。本轮的模型连接同样固定，避免用户编辑连接时把一种协议的工具参数发送到另一种协议的接口。模型生成完整参数后，工具服务检查路径、参数、版本与审批，再执行操作；结果以提供商原生工具结果格式返回模型继续生成正文。

支持 OpenAI Chat Completions、OpenAI Responses 和 Anthropic Messages。Gemini、DeepSeek、Ollama 等使用现有 OpenAI 兼容接口时，需要实际模型及服务支持 function calling。没有 `permissionMode` 的旧调用仍走文本回复接口。工具名称在上游映射为合法函数别名，返回时反查本次目录，再交给内部权限服务；模型不能通过别名绕过权限。

流式事件增加 `tool_call`、`approval_required`、`tool_result` 和 `content_snapshot`，聊天用可折叠的工具活动显示参数和结果，正文、思考和工具内容分别展示。每轮正式完成可修订流式草稿；后续工具或模型中断也会保留已修订的正文。Responses 的加密推理项及 Anthropic 的签名块只在当前上游续接中使用；正式聊天只保存已有公开思考文本及可见工具记录。

## 范围与权限

| 模式 | 自动执行 | 需要批准 |
|---|---|---|
| 请求批准 | 当前挂载内读取、查询；已发现技能的按需读取 | 写入、修改、删除、终端、范围外访问、MCP 调用 |
| 帮我批准 | 上述读取、挂载内可逆修改、已验证的 AppContainer 命令 | 删除、范围外访问、MCP 调用 |
| 完全访问权限 | 系统权限允许的文件操作及已启用工具 | 不弹逐次批准；范围外文件与 MCP 仍必须提供具体原因 |

默认使用挂载目录。完全访问不能提升为管理员，也不能关闭终端沙箱。文件工具不读取模型连接密钥或其备份，不直接改写正式聊天、记忆、索引和应用配置。其他正式 Data 文本可在说明原因后只读访问；Ask/Smart 需要批准。应用创建的 `Data/Desktop/Projects/<所属项目ID>` 是用户工作文件夹，在正式归属和精确目录匹配时允许正常工作文件操作；任意挂载 `Data/Chats` 等目录不会获得相同例外。

审批绑定聊天、请求、工具调用和单次令牌。参数在等待前冻结，批准不能替换路径或命令。拒绝、超时、关闭和取消都不执行该操作。批准后再次核对工作归属及配置版本；工作迁移或配置刷新使旧工具快照失效。MCP 自报的 `readOnlyHint` 等注解不改变审批策略。

## 文件与终端工具

`filesystem.list/read/search/stat/write/edit/delete/mkdir` 支持目录、UTF-8 文本和 SHA-256 版本检查。创建文件要求 `expectedHash: null`；替换、编辑和删除文件要求刚读取的准确 hash。编辑只替换唯一匹配，写入使用临时文件与原子替换。删除仅支持单个文件或空目录，不支持递归删除。拒绝链接、设备、网络共享、歧义路径和文件替代流；查询、结果、文件大小和遍历都有上限。

`terminal.run` 由独立 .NET `KYNXA.ToolHost` 创建 Windows AppContainer，使用 Job Object 管理子进程、256 MiB 内存上限、时间及输出限制。命令在临时工作副本中运行，没有网络能力，副本不会自动写回真实工作。正式 Data、模型凭据、链接和常见敏感/庞大目录不进入快照；精确匹配的应用管理工作目录仍能正常复制。完成后清理请求副本。

第一版支持 Node.js 及受限 `cmd` 内置命令。Node 脚本可使用内置模块，测试建议 `node --test --test-isolation=none`；默认子进程测试模式可能受沙箱限制。cmd 参数为 `['/d','/c','一条命令']`，已验证 `echo`、`type` 和副本内重定向；`dir` 的卷根探测可能被系统拒绝，目录和内容搜索请用文件工具。PowerShell、Python、任意可执行文件、调试端口、提权和安装流程尚不支持。启动或验证沙箱失败时返回错误，没有宿主执行回退。

AppContainer 是终端的系统隔离边界，Job Object 负责资源和生命周期。文件 CRUD 是网关的权限代理操作。用户启用的 MCP 服务程序是受信任的外部依赖，会以当前用户身份启动，**不在这个终端沙箱中**；不要把 MCP 调用审批描述成对服务程序本身的系统隔离。

## MCP 与 Skill

使用官方 TypeScript SDK 2.3.0 的 stdio 客户端，默认固定 `2025-11-25` 兼容协议；明确配置为 `2026-07-28` 时使用新版发现与协商。只运行用户保存并启用的程序和参数。打开窗口和普通列表查询不会启动服务；模型发起工具请求或点击“连接并刷新 MCP”才连接。配置禁用、删除、刷新和服务关闭会结束对应客户端。连接失败显示服务 ID 与安全错误码，其他可用工具仍保留。

应用技能来自打包的基础技能、`Data/Skills`、用户配置的技能目录，以及当前工作 `.kynxa/skills`。先提供有限的名字和用途，通过 `skill.list/read` 按需读取 SKILL.md。内置 `workspace-inspect` 与 `safe-file-edit` 引导读取、定位和带 hash 的精确修改。技能文本属于参考资料，不授予权限，不会自动执行脚本。仓库 `.agents/skills/kynxa-development` 是开发本项目时使用的编码技能，属于另一个用途。

## 数据与恢复

```text
Data/
├─ Agent/
│  └─ config.json                  MCP 程序/参数、启用状态、技能目录、配置 revision
├─ Skills/
│  └─ 技能目录/SKILL.md             用户应用技能
├─ Desktop/Projects/项目ID/        应用创建的用户工作文件
├─ Projects/项目ID/Sessions/聊天ID/events.jsonl
└─ Chats/聊天ID/events.jsonl        普通聊天正式记录
```

原聊天、记忆及索引结构见 [聊天与工作记忆](chat-work-memory.md)。ToolActivities 放在现有 assistant 消息里，由同一 `message.upsert` 日志保存。执行前保存 running，结束后保存结果；已完成请求重放回复，不再执行工具。中断或失败且已有工具记录的请求不能用同一请求 ID 自动重做，界面隐藏普通重试按钮；检查结果后发送新消息。它不是跨崩溃精确续跑或多文件事务系统。

初始化自动建立 Agent/Skills，设置迁移会复制验证它们。外部工作目录、外部技能目录和 MCP 可执行程序仍是用户配置的路径，不假定另一台电脑存在相同位置。配置损坏保留原文件并报错，revision 冲突不覆盖其他窗口的新配置。

## 验证入口

- 网关：`node --test tests/*.test.mjs`，从 `apps/model-gateway` 运行。记忆效果测试依据模拟模型实际收到的 system 回答；工具测试读取真实临时工作文件、续接三种协议、检查审批身份、配置冲突及重放去重。MCP 测试启动官方 SDK 的本地测试服务并真实发现/调用两个协议版本，另验证模型声明、SDK 调用、结果续接的完整链路。迁移清理失败不会产生未处理拒绝，health 以固定错误码报告失败。
- 客户端：`tests/agent-client-smoke`、`tests/chat-stream-smoke`、`tests/storage-migration-smoke`、`tests/gateway-startup-smoke`。
- 界面：`tests/agent-ui-smoke` 的独立原生窗口与 `tests/agent-transcript-smoke` 的独立浏览器 DOM 验收；都不使用实际用户数据。
- 系统沙箱：`tests/sandbox-smoke` 实际检查副本读写、外部文件拒绝、loopback 网络拒绝、父子进程取消、输出和内存限额。不是仅检查函数返回的 sandbox 字段。

这些验证不保证所有模型会正确选择工具，也不代表所有系统版本、DPI 或终端命令已经兼容。

本轮验收：网关 207/207、工具客户端 21/21、工具展示 DOM 15/15、独立原生窗口和审批 53/53 通过；从无 `node_modules` 的独立源码副本构建桌面，0 警告、0 错误，输出内 SDK 可正常导入，自包含 ToolHost 通过实际系统沙箱检查。测试使用临时数据和模拟模型，不调用用户付费接口或改动当前打开的应用数据。

实现依据：[OpenAI 函数调用](https://developers.openai.com/api/docs/guides/function-calling)、[Claude 工具结果](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)、[MCP SDK v2](https://ts.sdk.modelcontextprotocol.io/v2/)、[MCP 协议版本](https://ts.sdk.modelcontextprotocol.io/v2/protocol-versions)、[Microsoft AppContainer](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)、[Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)。
