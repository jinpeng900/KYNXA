# KYNXA 1.7

<p align="center">
  <a href="./README.md">English</a> | 中文
</p>

**KYNXA** 是面向 Windows 的本地优先个人 Agent 应用，将云端与本地模型、长任务、文档检索、工具，以及文件／网页／终端工作区整合在桌面中。

**首个正式版：`1.7`（安装包版本 `1.7.0`）· Windows x64**

1.7 是 KYNXA 的首个正式版，整合长任务续跑与恢复、有范围隔离的记忆和工具、检索与缓存复用、桌面操作，以及文档生成和预览。本地检索改用多语言 E5 加词法检索、RRF 名次融合与轻量排序，替代 BGE。DOCX／PPTX／XLSX 原格式预览、原生 Office 文档生成和终端执行回执均继续保留。版本变化见 [1.7 更新说明](docs/releases/1.7.md)，安装入口见 [1.7 发布页](https://github.com/jinpeng900/KYNXA/releases/tag/v1.7.0)，架构与使用入口见 [文档索引](docs/README.md)。

**成品与源码说明：** 从源码构建已发布版本时，请使用对应的发布标签；不同分支或修订可能包含不同实现。

验证采用 Rust、HTTP／SSE 和 Windows UI 定向检查，覆盖实际执行的路径，不等于所有模型、网站、安装场景或大库检索质量均已覆盖；历史验证结果保留在 [1.5.3 补丁说明](docs/releases/1.5.3.md)。

## 1.7 成品能力

| 领域 | 已接入的功能 |
| --- | --- |
| 聊天与模型 | 流式回复、中止、持久历史、切换服务商和模型；支持 Chat Completions、Responses、Claude Messages 与 Ollama 兼容连接。 |
| 长任务 | 稳定任务身份、检查点、暂停恢复、消息排队、中途引导、子代理、产物及 token／耗时统计。引导后新回复接在补充消息之后，前面的输出保留。 |
| 项目 | 一个项目关联多个文件夹，按工作范围检索代码与文件，增量更新索引，并在项目激活后后台准备。 |
| 记忆 | 聊天、工作／项目、用户三个范围；候选、确认、修改和删除。按当前问题和授权范围选择有效的已确认记忆，不把整个记忆库塞入每轮上下文。 |
| 工具经验 | 持久工具目录，以及已结算调用的成败和耗时经验。配置、schema、信任、禁用或删除变化时更新相应条目；普通聊天不增加工具记忆调用。 |
| 文档与检索 | 按路径直接读取和附件解析；支持 PDF、DOC／DOCX、PPT／PPTX、表格、Markdown／文本／代码、图像 OCR、压缩包及网站来源。后台索引、E5／词法 RRF、轻量排序与来源引用共用有范围隔离的资料库；明确路径或符号可直接使用 glob／正则搜索。 |
| 文档生成 | 原生 `document.create` 生成真正可编辑的 DOCX、PPTX、XLSX；PDF 导出复用随包 Office 转换器。写入返回已核验的文件大小、哈希及路径；XLSX 公式保存显式提供的缓存值，不自行执行计算。 |
| 右侧工作区 | 文件与附件的 DOCX／PPTX／XLSX 直接预览，缓存文件标签，Markdown／公式／Mermaid，代码高亮，图片／SVG／隔离 HTML，Git 未提交变更及多个内嵌终端。 |
| 网站 | 按需启动随包 Chromium，直接输入网址或关键词，前进／后退／刷新、网页交互，以及应用持久浏览器配置。 |
| 本地模型 | 设置中的模型目录、安装、导入、暂停／继续下载、启停、删除和存储位置选择；启用后进入正常聊天模型选择器。 |
| 技能与扩展 | 工具任务按相关性选择技能简介，需要时再读取完整说明；原生工具、stdio／Streamable HTTP MCP 与 ToolHost 提供桌面、终端、沙箱和原生执行。普通聊天不扫描无关技能目录。 |
| 终端执行 | `terminal.workspace.run` 在可见的内嵌终端提交命令或脚本，返回命令 ID、实际输出、完成状态和退出码；后续读取观察同一次命令，不重复执行。 |
| 资源控制 | 代理分配的沙箱 CPU／内存额度落实到 Windows Job，执行前回读核验；沙箱及宿主后台任务日志有界保留，超量后继续排空。 |

DOCX 先显示可操作页面，嵌入字体和图片在后台补齐；长文档在预览内部自然滚动，并保留阅读位置。PPTX 支持翻页与缩放；XLSX 支持工作表切换、类型化数值、常用样式、合并单元格、公式缓存和大表可见区域渲染。切回已缓存文件复用原有 DOM，切换标签后后台准备仍可继续；实际不支持的 Office 特性及旧格式保留 PDF 转换路径。

正文提取、文档生成与版式预览分别处理。提取文字不等于完整恢复排版，表格预览也不会重新计算公式；各格式的实际范围见 [文档解析说明](docs/architecture/document-ingestion.md)。网站登录、验证及媒体可用性仍由网站和浏览器能力共同决定。

完整运行资源包含用于检索的多语言 **E5 嵌入模型**，与对话生成模型分开。E5 与词法候选通过 **RRF 名次融合**后进行轻量结构化排序，**不再随包提供或依赖 BGE**，已有 E5 索引继续复用。查询、检索及模型前缀缓存减少重复工作，范围与来源版本检查保持有效。大库容量、ANN 就绪衔接与检索质量分别记录验证范围，详见 [检索架构](docs/architecture/retrieval.md)。

## 安装与数据位置

Windows x64 正式版已在 [1.7 发布页](https://github.com/jinpeng900/KYNXA/releases/tag/v1.7.0)发布，下载入口如下：

| 下载 | 用途 |
| --- | --- |
| [安装包 ZIP](https://github.com/jinpeng900/KYNXA/releases/download/v1.7.0/KYNXA-1.7.0-windows-x64-setup.zip) | 解压后运行其中的 EXE，安装软件及完整基础运行资源。 |
| [运行资源整合包](https://github.com/jinpeng900/KYNXA/releases/download/v1.7.0/KYNXA-1.7.0-windows-x64-resources.zip) | 为源码构建提供运行资源，不能单独当作软件运行，也不包含开发 SDK。 |

EXE 安装器支持软件、数据、用户工具位置，中英文界面、桌面快捷方式，以及根据硬件推荐的可选本地对话模型。完整基础包包含原生服务、E5、文档转换组件、私有 Node／MCP 依赖和 Chromium；勾选的对话模型在首次启动后通过软件内模型管理器下载。

1.7 公开分发仅安装包 ZIP 与运行资源 ZIP，不单独提供安装 EXE 或在线安装器；源码中的在线安装构建工具仍可供维护者使用。

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

安装包 ZIP 内的 EXE、安装选项和升级行为见 [打包指南](apps/desktop/Build/Installer/README.md)。

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

源码仓库保留当前文档、代码、清单与准备脚本。本版通过 Release 附件分发安装包 ZIP 和运行资源 ZIP；验证产物不进入源码版本管理，历史设计压缩包不是当前构建或安装的前置条件。

常用入口：

* [文档索引](docs/README.md) 与 [1.7 更新说明](docs/releases/1.7.md)
* [核心指南](apps/agent-core/README.md) 与 [原生架构](docs/architecture/native-gateway.md)
* [聊天与工作记忆](docs/architecture/chat-work-memory.md)
* [文档解析](docs/architecture/document-ingestion.md) 与 [检索](docs/architecture/retrieval.md)
* [桌面组件](apps/desktop/UI-COMPONENTS.md)
* [五人职责](docs/team/README.md)、[模块边界](docs/architecture/team-boundaries.md) 与 [GitHub 协作](docs/team/github-collaboration.md)

## 协作与验证

仓库开发遵循 `AGENTS.md` 引导的 [开发 Skill](.agents/skills/kynxa-development/SKILL.md) 与 [代码规范 Skill](.agents/skills/kynxa-code-standards/SKILL.md)，覆盖当前语言、模块职责、命名、双语说明注释、兼容规则与最低必要验证。开发代理规则与应用内模型技能分开管理。

根据改动选择必要检查，保留已有本地改动和正式数据，将本轮结果与历史评测分开。源码接线、编译、Windows 交互、检索质量和安装器执行分别记录，具体命令见 [验证指南](.agents/skills/kynxa-development/references/validation.md)。

反馈问题时提供受影响的正式版本、操作步骤和脱敏日志，不在 Issue 或贡献内容中包含凭据、私有聊天、记忆及个人浏览器数据。

## 许可证

KYNXA 使用 [Apache License 2.0](LICENSE)。随包第三方组件与模型资产保留各自的许可证和声明。
