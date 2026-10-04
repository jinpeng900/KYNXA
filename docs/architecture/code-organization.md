# 当前代码组织与维护边界

更新：2026-10-05。本文件描述已经运行的 C#/WinUI → Node.js 网关 → 模型接口及原生工具链。根 README 中的 Rust Authority、持久任务图和跨崩溃精确续跑仍是后续设计，不能用它们解释当前代码。

## 目录与职责

```text
apps/
├─ desktop/
│  ├─ Views/                 主窗口布局、事件接线、聊天与侧栏可见状态
│  ├─ Controls/              可复用控件、WebView2 聊天与图片预览
│  ├─ ViewModels/、Models/   展示对象、稳定身份和界面状态
│  ├─ Services/              API、流读取、解析、路径迁移和纯状态计算
│  ├─ Resources/Transcript/  随包 Markdown/公式/代码和活动展示
│  └─ Build/                 独立运行时载荷、校验和许可证打包
├─ shared/                   纯 DTO 与校验，不依赖窗口或网络
├─ model-gateway/            正式会话、记忆、上下文、模型和工具服务
│  └─ official-tools/        随应用发布的工具、技能和 MCP 预设
├─ tool-host/                Windows AppContainer、宿主终端、桌面操作
├─ desktop-preview/         开发预览与 API 代理，不是第二份正式前端
└─ mock-backend/             旧合同的固定回复测试服务
tests/                       隔离数据、模拟模型和自有窗口的回归检查
.agents/skills/              开发仓库的 Skill，区别于应用模型技能
```

目录名称不是独立性证明。`ShellPage.*` 仍是共享页面状态的 partial 类；`ProjectStore` 实际是会话 API 客户端。不能因文件拆开就宣称已经完成完整 MVVM，也不能另建桌面正式日志绕开网关。

## 主要调用链

| 链路 | 所属模块与合同 |
|---|---|
| 发送、切换、流式展示 | Shell 接线 → Chat/Agent API 客户端 → `ChatStreamReader` → 展示状态；UI 只接收当前身份的结果 |
| 正式聊天与确认记忆 | 网关 `runtime` → `conversations` / `memory-service` → 仓储；JSONL 和来源版本保持权威 |
| 模型历史与预算 | `context`、`model-history`、`output-budget` 和模型协议适配；只压缩请求投影，调用与结果保持配对 |
| 工具执行 | `ToolService` 固定范围、参数与审批，分派文件/MCP/技能/原生执行，保存正式结果再继续模型循环 |
| 路径保护 | `ToolStorageBoundary` 解析统一存储身份和别名；它只判定路径，不授予执行权限 |
| 模型能力提示 | `buildToolSystemPrompt` 根据本请求的能力及技能快照生成有界提示；不会执行技能或改变配置 |
| 截图展示 | 正式工具引用 → `ConversationScreenshotSources` → 加载器 → 标签状态和图片控件；预览不能替代正式结果 |
| MCP 配置编辑 | 视图收集输入，`McpConfigurationInput` 做纯编辑校验，API 保存配置；后端仍负责最终校验 |
| 本机前后台窗口 | 原生启动/窗口操作 → `DesktopForegroundPlacement` → 真实观测回执；读取/截图与前台输入分开 |
| 背景浏览器参数 | `desktop-launch-options.mjs` 在审批前准备 Chrome/Edge 的防遮挡限流参数；其他软件、明确前台模式和既有浏览器进程不改动 |
| 构建与部署载荷 | 项目声明资源清单，`Build/RuntimePackaging.targets` 处理独立 Node/ToolHost 和 PRI/许可流程 |
| 开发预览 API | `server.mjs` 管静态文件与启动，`preview-api-proxy.mjs` 管流、背压与取消；正式数据仍归网关 |

## 本轮修缮的边界

- 工具提示和存储路径策略从执行协调服务分离；预算、权限和来源 ID 保持原合同。
- 工具关闭失败仍清理其他所有者；模型运行时等候在途请求和已执行回执结算，错误保留给调用者。
- 非法工具输入返回有界错误，避免错误分支再次访问无效参数导致异常逃逸。
- Transcript 后台解析使用已捕获的缓存引用；关闭时使旧代次失效。注入的工具客户端由创建方释放。
- 闭合标签保留有界状态，短暂空投影不重开标签；浏览器布尔参数按实际值解析。
- 后台软件和独立终端保持原窗口状态，不以最小化换取不遮挡。截图取目标窗口，DOM/辅助功能读取与物理输入分开。
- 预览代理保留 API 查询、真实响应类型和增量输出；断开取消上游，坏 URI 返回错误而不终止服务。

终端不投影到正式右侧栏目；真实执行与协议事件、归档仍保留。独立终端控件及其隔离夹具不表示当前产品开启了右侧终端功能。

## 持续开发约束

公开网页读取另由 `web-http-transport.mjs` 负责请求、地址校验、重定向、压缩、大小及取消，`web-fetch.mjs` 复用上游解析器转换文本并生成短页和完整归档；官方目录仅引用声明，`ToolService` 继续拥有权限与回执。文件分页保留在文件模块。`tool-system-prompt.mjs` 按运行时预留的声明空间选择技能摘要，未展示技能由原目录按需加载。

遵循 [开发 Skill](../../.agents/skills/kynxa-development/SKILL.md) 和 [代码规范](../../.agents/skills/kynxa-development/references/coding-standards.md)。新行为放到实际拥有该职责的模块，不用任意行数上限驱动重写。

异步 I/O 传递取消信号，UI 回写检查会话、请求和代次；后台任务观察错误。缓存有容量和失效规则，外部客户端、进程、事件和流由创建者释放。共享 DTO 不引用 WinUI，网关不依赖桌面程序集。

原始记录不因界面隐藏或上下文预算删除；身份、来源隔离、审批快照和版本冲突保持原合同。开发与回归只使用临时数据和模拟凭据。正式安装、真实账号浏览器、付费模型效果及所有第三方软件的前台行为需要各自验收，不由模拟测试代替。

验证命令见 [验证与协作](../../.agents/skills/kynxa-development/references/validation.md)、[网关结果](../../apps/model-gateway/README.md#当前验证与后续接口) 和 [打包说明](../../apps/desktop/Build/README.md)。本轮是代码组织、执行可靠性和前台体验检查，不是安全认证或后续完整 Host 架构的实现。
