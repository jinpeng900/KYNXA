# KYNXA 1.6

<p align="center">
  <a href="./README.md">English</a> | 中文
</p>

**KYNXA** 是面向 Windows 的本地优先个人 Agent 应用，将云端与本地模型、长任务、文档检索、工具，以及文件／网页／终端工作区整合在桌面中。

**桌面发行版本：`1.6`（安装包版本 `1.6.0`）· Windows x64 · 持续开发中**

1.6 增加 DOCX／PPTX／XLSX 原格式预览、可操作首屏与后台资源准备、原生 Office 文档生成，以及更准确的终端执行回执。成品下载见 [1.6 发布页](https://github.com/jinpeng900/KYNXA/releases/tag/v1.6.0)，架构与使用入口见 [文档索引](docs/README.md)。

**成品与源码说明：** 1.6 成品包包含尚未推送到 `main` 的本地实现。本次 README 更新不包含这些功能源码，因此直接拉取当前 GitHub 源码构建，可能尚不具备下列全部 1.6 成品功能。

本轮 **89 项 Windows UI 定向检查、6 项 Rust 工作区预览测试通过**，Debug Full 桌面 **零警告、零错误构建**。这些结果针对本次预览及交互路径，不等于所有模型、网站、安装场景或大库检索质量均已覆盖；历史验证结果保留在 [1.5.3 补丁说明](docs/releases/1.5.3.md)。

## 1.6 成品能力

| 领域 | 已接入的功能 |
| --- | --- |
| 聊天与模型 | 流式回复、中止、持久历史、切换服务商和模型；支持 Chat Completions、Responses、Claude Messages 与 Ollama 兼容连接。 |
| 长任务 | 稳定任务身份、检查点、暂停恢复、消息排队、中途引导、子代理、产物及 token／耗时统计。引导后新回复接在补充消息之后，前面的输出保留。 |
| 项目 | 一个项目关联多个文件夹，按工作范围检索代码与文件，增量更新索引，并在项目激活后后台准备。 |
| 记忆 | 聊天、工作／项目、用户三个范围；候选、确认、修改和删除。按当前问题和授权范围选择有效的已确认记忆，不把整个记忆库塞入每轮上下文。 |
| 工具经验 | 持久工具目录，以及已结算调用的成败和耗时经验。配置、schema、信任、禁用或删除变化时更新相应条目；普通聊天不增加工具记忆调用。 |
| 文档与检索 | 按路径直接读取和附件解析；支持 PDF、DOC／DOCX、PPT／PPTX、表格、Markdown／文本／代码、图像 OCR、压缩包及网站来源。后台索引、词法／向量检索、来源引用和可选重排共用有范围隔离的资料库。 |
| 文档生成 | 原生 `document.create` 生成真正可编辑的 DOCX、PPTX、XLSX；PDF 导出复用随包 Office 转换器。写入返回已核验的文件大小、哈希及路径；XLSX 公式保存显式提供的缓存值，不自行执行计算。 |
| 右侧工作区 | 文件与附件的 DOCX／PPTX／XLSX 直接预览，缓存文件标签，Markdown／公式／Mermaid，代码高亮，图片／SVG／隔离 HTML，Git 未提交变更及多个内嵌终端。 |
| 网站 | 按需启动随包 Chromium，直接输入网址或关键词，前进／后退／刷新、网页交互，以及应用持久浏览器配置。 |
| 本地模型 | 设置中的模型目录、安装、导入、暂停／继续下载、启停、删除和存储位置选择；启用后进入正常聊天模型选择器。 |
| 技能与扩展 | 工具任务按相关性选择技能简介，需要时再读取完整说明；原生工具、stdio／Streamable HTTP MCP 与 ToolHost 提供桌面、终端、沙箱和原生执行。普通聊天不扫描无关技能目录。 |
| 终端执行 | `terminal.workspace.run` 在可见的内嵌终端提交命令或脚本，返回命令 ID、实际输出、完成状态和退出码；后续读取观察同一次命令，不重复执行。 |
| 资源控制 | 代理分配的沙箱 CPU／内存额度落实到 Windows Job，执行前回读核验；沙箱及宿主后台任务日志有界保留，超量后继续排空。 |

DOCX 先显示可操作页面，嵌入字体和图片在后台补齐；长文档在预览内部自然滚动，并保留阅读位置。PPTX 支持翻页与缩放；XLSX 支持工作表切换、类型化数值、常用样式、合并单元格、公式缓存和大表可见区域渲染。切回已缓存文件复用原有 DOM，切换标签后后台准备仍可继续；实际不支持的 Office 特性及旧格式保留 PDF 转换路径。

正文提取、文档生成与版式预览分别处理。提取文字不等于完整恢复排版，表格预览也不会重新计算公式；各格式的实际范围见 [文档解析说明](docs/architecture/document-ingestion.md)。网站登录、验证及媒体可用性仍由网站和浏览器能力共同决定。

完整运行资源包含多语言 **E5 嵌入模型**与 **BGE 重排模型**，两者用于检索，不是对话生成模型。重排按需启用，与原检索排名融合，不假定每个问题都会提升。大库容量、ANN 就绪衔接与检索质量分别记录验证范围，详见 [检索架构](docs/architecture/retrieval.md)。

## 安装与数据位置

所有下载均作为附件发布于 [1.6 发布页](https://github.com/jinpeng900/KYNXA/releases/tag/v1.6.0)：

| 下载 | 用途 |
| --- | --- |
| [完整离线安装包](https://github.com/jinpeng900/KYNXA/releases/download/v1.6.0/KYNXA-1.6.0-windows-x64-setup.exe) | 安装软件及完整基础运行资源。 |
| [在线安装包](https://github.com/jinpeng900/KYNXA/releases/download/v1.6.0/KYNXA-1.6.0-windows-x64-online-setup.exe) | 安装时下载并校验完整运行载荷。 |
| [安装器压缩包](https://github.com/jinpeng900/KYNXA/releases/download/v1.6.0/KYNXA-1.6.0-windows-x64-setup.zip) | 解压后运行其中的安装器。 |
| [运行资源整合包](https://github.com/jinpeng900/KYNXA/releases/download/v1.6.0/KYNXA-1.6.0-windows-x64-resources.zip) | 为源码构建提供运行资源，不能单独当作软件运行，也不包含开发 SDK。 |

EXE 安装器支持软件、数据、用户工具位置，中英文界面、桌面快捷方式，以及根据硬件推荐的可选本地对话模型。完整基础包包含原生服务、E5／BGE、文档转换组件、私有 Node／MCP 依赖和 Chromium；勾选的对话模型在首次启动后通过软件内模型管理器下载。

在线安装器自动选择已配置来源、校验完整载荷并保留可续传的下载断点，不要求用户选择镜像或运行下载脚本。

**使用完整安装包的普通用户不需要安装开发 SDK、Rust、Node、Python 或手动配置 PATH。** 云端模型凭据及需要账号的第三方服务仍由用户配置。当前安装器构建尚未配置生产代码签名，生成的 EXE 尚未进行安装运行测试，详见 [安装打包说明](apps/desktop/Build/Installer/README.md)。

新的无包身份安装默认使用软件旁固定的 `Data/` 与 `Extensions/`，默认软件位置为当前用户可写目录。安装时或设置中可以更改位置，升级继续保留有效存储指针。相对位置随完整软件目录移动，单独选择的外部位置保持配置路径；已有 AppX／MSIX 安装继续使用既有持久用户目录。详见 [目录与迁移](docs/architecture/native-gateway.md)。

模型连接与本地模型清单保存在配置的模型根目录，权重默认位于其 `Local/`。网站登录状态使用应用自己的持久浏览器配置，跨聊天和重启复用；关闭标签不清除登录数据，用户可以主动清除站点数据。应用不借用个人浏览器配置。

## 从源码构建

开发机需要 **Windows x64**、受支持的 **.NET 10 SDK**、**Rust MSVC 工具链**，以及包含 **Windows SDK 的 Visual Studio C++ 构建工具**。这些是源码开发要求，安装成品软件的用户无需搭建。

在仓库根目录运行：

```powershell
# 使用已安装的开发工具链准备运行资源、构建并启动。
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Run

# 只构建，不启动。
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Build
```

已有完整运行资源包可传入 `-ResourcePackRoot <ZIP路径>`；离线源码构建还需要对应 SDK、Cargo／NuGet 缓存，运行资源 ZIP 不能替代开发环境。已准备好的资源会继续复用。构建档位、下载源与离线选项见 [构建指南](apps/desktop/Build/README.md)。

完整与在线 EXE 的形式、安装选项和升级行为见 [打包指南](apps/desktop/Build/Installer/README.md)。

## 当前架构

```text
C# / WinUI 桌面 + WebView 展示
                 │ 本机 HTTP / SSE
                 ▼
Rust 网关与 Agent 核心
  ├─ 任务编排、上下文、检查点与子代理
  ├─ 云端 API / 受管理的本地 llama.cpp / 外部模型服务
  ├─ 工具、MCP、Skill、网页检索与 Chromium/CDP
  │    └─ C# ToolHost：Windows 桌面 / 终端 / 沙箱 / 原生操作
  ├─ 有范围隔离的检索、原生 ONNX 推理与后台索引
  └─ 正式 JSONL / JSON / SQLite 存储
                 │
                 ▼
独立 Rust 硬件资源服务
```

Rust 拥有正式 HTTP／SSE 后端、模型与工具 I/O、任务状态、检索和存储事务；C# 负责 Windows UI 和 ToolHost 集成。WebView JavaScript 展示文档和聊天，私有 Node 用于扩展兼容；核心不依赖 Node 或 JavaScript 网关。

原始聊天事件、稳定 ID、工具回执及来源版本是正式依据。界面缓存和摘要用于展示与恢复，不替代原始记录；敏感操作继续经过现有权限与审批边界。详见 [原生网关](docs/architecture/native-gateway.md)、[持久任务](docs/architecture/durable-tasks.md) 和 [Agent 工具](docs/architecture/agent-tools.md)。

## 仓库与文档

| 路径 | 职责 |
| --- | --- |
| `apps/desktop/` | WinUI、WebView 展示、桌面客户端、构建与安装脚本。 |
| `apps/agent-core/` | Rust 网关，以及编排、模型、工具、检索、存储和资源客户端。 |
| `apps/resource-service/` | 原生硬件观测与资源准入。 |
| `apps/tool-host/` | Windows 桌面、终端、沙箱和原生执行。 |
| `apps/shared/` | C# 聊天、记忆、检索与工具契约。 |
| `apps/official-tools/` | 官方静态技能、工具预设及许可证原文。 |
| `docs/` | 持续维护的 Markdown 指南、架构、版本说明及团队文档。 |
| `tests/` | 按需使用的桌面、存储、协议、打包和交互检查。 |

源码仓库保留当前文档、代码、清单与准备脚本。EXE 和运行资源／模型／浏览器压缩包通过 Release 附件分发；验证产物不进入源码版本管理，历史设计压缩包不是当前构建或安装的前置条件。

常用入口：

* [文档索引](docs/README.md) 与 [1.5 更新说明](docs/releases/1.5.md)
* [核心指南](apps/agent-core/README.md) 与 [原生架构](docs/architecture/native-gateway.md)
* [聊天与工作记忆](docs/architecture/chat-work-memory.md)
* [文档解析](docs/architecture/document-ingestion.md) 与 [检索](docs/architecture/retrieval.md)
* [桌面组件](apps/desktop/UI-COMPONENTS.md)
* [五人职责](docs/team/README.md)、[模块边界](docs/architecture/team-boundaries.md) 与 [GitHub 协作](docs/team/github-collaboration.md)

## 协作与验证

仓库开发遵循 `AGENTS.md` 引导的 [开发 Skill](.agents/skills/kynxa-development/SKILL.md) 与 [代码规范 Skill](.agents/skills/kynxa-code-standards/SKILL.md)，覆盖当前语言、模块职责、命名、双语说明注释、兼容规则与最低必要验证。开发代理规则与应用内模型技能分开管理。

根据改动选择必要检查，保留已有本地改动和正式数据，将本轮结果与历史评测分开。源码接线、编译、Windows 交互、检索质量和安装器执行分别记录，具体命令见 [验证指南](.agents/skills/kynxa-development/references/validation.md)。

KYNXA 持续开发中。反馈问题时提供受影响版本、操作步骤和脱敏日志，不在 Issue 或贡献内容中包含凭据、私有聊天、记忆及个人浏览器数据。

## 许可证

KYNXA 使用 [Apache License 2.0](LICENSE)。随包第三方组件与模型资产保留各自的许可证和声明。
