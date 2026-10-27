# KYNXA 1.8.6

<p align="center"><a href="./README.md">English</a> | 中文</p>

KYNXA 是 Windows 个人智能体助手，将聊天、文件、工具和持续工作整合在同一个桌面应用中。

## 可以做什么

- 连接云端 API，或使用支持的本地模型。
- 处理多步骤任务，接受中途引导，暂停后继续已有工作。
- 检索项目文件与资料，使用限定范围的记忆和来源引用。
- 创建和预览文档、表格、演示文稿、图片、网页、音视频及支持的三维模型。
- 调用 MCP 服务、应用技能、浏览器工具和本机终端。
- 在右侧工作区查看文件、网页及终端会话。
- 主窗口隐藏后，通过系统托盘继续运行。

## 下载

**版本 1.8.6 · Windows x64**

- [下载 EXE 安装包](https://github.com/jinpeng900/KYNXA/releases/download/v1.8.6/KYNXA-1.8.6-windows-x64-setup.exe)
- [运行资源包](https://github.com/jinpeng900/KYNXA/releases/download/v1.8.5/KYNXA-1.8.5-windows-x64-resources.zip)
- [发布页与更新内容](https://github.com/jinpeng900/KYNXA/releases/tag/v1.8.6)

直接运行 EXE 安装。资源 ZIP 用于准备源码构建环境，不是独立软件。
完整安装后无需安装开发 SDK 或手动配置 PATH。云端模型和部分外部服务需要自行配置账号或凭据。

## 从源码构建

使用 Windows x64，准备 Git、.NET 10 SDK（10.0.100 或更高的 10.0 SDK）、Rust 1.88 及以上的 MSVC 工具链，以及包含 Windows SDK 的 Visual Studio C++ 构建工具。

```powershell
git clone https://github.com/jinpeng900/KYNXA.git
cd KYNXA
git checkout v1.8.6
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Run -Configuration Debug -Profile Full
```

只构建、不启动时改用 `-Action Build`。已有缓存会复用；离线构建还需预先准备 SDK、Cargo/NuGet 依赖，并通过 `-Offline -ResourcePackRoot "资源包路径.zip"` 指定兼容资源包。

## 许可证

[Apache License 2.0](LICENSE)。随包第三方组件保留各自的许可证和声明。
