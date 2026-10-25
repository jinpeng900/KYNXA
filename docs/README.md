# KYNXA 1.8.1 正式版文档

当前版本为 Windows x64 **1.8.1**，安装发行版本为 **1.8.1**。版本变化与定向验证见 [1.8.1 更新说明](releases/1.8.1.md)，EXE 安装包及运行资源包见 [1.8.1 发布页](https://github.com/jinpeng900/KYNXA/releases/tag/v1.8.1)。功能与架构以所用版本和下面的维护文档为准；带日期的测试和迁移记录保留各自当时的环境、版本和结果。

1.7 首个正式版的产品定位、分发文件和验证边界保留在 [1.7 更新说明](releases/1.7.md)。该版和 [1.5.3 更新说明](releases/1.5.3.md#验证范围与边界)中的构建、恢复、资源配额和打包结果属于历史记录，不改记为本版成绩。

## 使用与开发入口

| 内容 | 入口 |
| --- | --- |
| 项目介绍、快速开始 | [项目 README](../README.md) |
| 本版变化与验证范围 | [1.8.1 更新说明](releases/1.8.1.md) |
| 历史版本 | [1.8 更新说明](releases/1.8.md)、[1.7.1 更新说明](releases/1.7.1.md)、[1.7 首个正式版](releases/1.7.md)、[1.5 系列总览](releases/1.5.md)、[1.5.3 更新说明](releases/1.5.3.md) |
| 源码环境、增量构建与启动 | [桌面构建说明](../apps/desktop/Build/README.md) |
| EXE 安装器、目录与发布载荷 | [安装器说明](../apps/desktop/Build/Installer/README.md) |
| 完整离线安装、原站／GitHub／国内源与断点恢复 | [离线安装与多来源下载](architecture/offline-installer.md) |
| 开发规则、必要验证 | [开发 Skill](../.agents/skills/kynxa-development/SKILL.md)、[代码规范 Skill](../.agents/skills/kynxa-code-standards/SKILL.md) |
| 成员分工与协作 | [团队入口](team/README.md)、[GitHub 协作](team/github-collaboration.md) |

## 当前架构

| 主题 | 文档 |
| --- | --- |
| Rust 网关、桌面通信、运行目录与随包资源 | [原生网关](architecture/native-gateway.md) |
| 模块所有者、依赖与跨端合同 | [团队边界](architecture/team-boundaries.md)、[主责清单](team/module-ownership.json) |
| 本地检索、E5／词法 RRF、ANN 与增量索引 | [检索架构](architecture/retrieval.md) |
| 本地对话模型安装、导入、启用和资源策略 | [本地模型管理](architecture/local-models.md) |
| 文档、图片与离线 OCR | [文档入库](architecture/document-ingestion.md) |
| 压缩文件和网站链接解析 | [压缩与网址入库](architecture/archive-and-url-ingestion.md) |
| 工具、MCP、Skill、浏览器与工具记忆 | [工具架构](architecture/agent-tools.md) |
| 用户、工作、聊天记忆及确认 | [聊天与工作记忆](architecture/chat-work-memory.md) |
| 长任务、子任务、暂停与恢复 | [持久任务](architecture/durable-tasks.md)、[任务结果](architecture/task-outcomes.md) |

## 文档维护

当前开发说明直接维护为 Markdown；旧设计文档包及其生成的压缩包不再随源码仓库分发，也不作为构建依赖。安装程序、运行资源包和模型权重属于发布或分发产物，获取方式见构建与安装器说明。

`architecture/` 中带日期的研究、迁移和评测报告，以及 `verification/` 中的历史验收记录，用于追溯；其中的旧语言入口、测试数字、第三方依赖版本和安装包文件名不随本版机械改写。使用当前功能时优先查看上表入口。
