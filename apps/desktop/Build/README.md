# 桌面运行时打包

`KYNXA.Desktop.csproj` 定义桌面平台、依赖、共享合同和明确的发布资源清单；通过 `RuntimePackaging.targets` 接入独立执行载荷的构建流程。这里不读取用户的 Data、模型凭据或自定义扩展。

原生 HTTP/SSE 网关通过 `prepare-native-runtime.ps1` 构建 `runtime/agent/kynxa-gateway.exe`，资源服务为 `runtime/resource/kynxa-resource-service.exe`；Debug/Release 都验证源码指纹及资产 SHA-256。核心不依赖 Node、NAPI 或 JavaScript 网关；安装包在独立 `runtime/mcp` 目录随附固定私有 Node 和内置 MCP 依赖，WebView2 前端脚本保留。用户不需要安装 Node、配置系统 PATH 或运行 npx。源码构建需要 Rust 1.88+ 和 MSVC，现有 dotnet run 命令继续使用。

| 文件 | 职责 |
|---|---|
| `RuntimePackaging.targets` | 加入原生网关、资源服务、ToolHost、本地嵌入与重排模型、私有 MCP 运行时、原始许可证及 PRI 处理 |
| `native-assets/*.json` | 固定 ORT 1.21.0、DirectML 1.15.4、sqlite-vec 0.1.9、模型与许可的原始下载地址、长度和 SHA-256 |
| `prepare-native-runtime.ps1` | PowerShell 5.1 构建/校验原生载荷，复用 Cargo 与模型缓存；下载归档只提取白名单 DLL/许可，不执行 SDK 安装代码 |
| `prepare-mcp-runtime.ps1` / `native-assets/mcp-runtime.json` | 构建机校验官方 Node 24.14.0 ZIP 和执行文件 SHA-256，按固定 package-lock 安装内置 MCP 与对应 Chromium；不运行依赖安装脚本，不随包携带 npm/npx |
| `prepare-toolhost-notices.ps1` | 从实际恢复的 .NET 运行时包复制原许可证和第三方声明 |
| `vc-runtime.json` / `prepare-vc-runtime.ps1` | 从固定正式 Microsoft Redist 获取并验证 x64 app-local VC 运行库及许可；生成明确发布清单，不复制 System32 DLL |
| `filter-runtime-pri-layout.ps1` | 将独立运行时保留为普通包载荷，不让 WinUI 把它们解释成界面资源 |
| `remove-obsolete-backend-payload.ps1` | 仅在验证后的桌面输出中移除精确旧 Node/NAPI/网关载荷；正式数据和链接路径拒绝清理 |

所有项目路径以 `MSBuildProjectDirectory` 和项目内相对路径解析，输出缓存位于项目的中间目录或忽略的 `artifacts/runtime`。不根据开发机用户名、盘符或安装 SDK 的猜测版本定位运行时。嵌入权重和 VC 文件仅按验证后的发布清单复制，缓存杂项不进入安装包。模型资产清单位于 `Build/native-assets`；官方只读资料位于 `apps/official-tools`。实际发布根据 bundle-files.txt 白名单，不将下载归档、旧 Node 目录或缓存杂项打包。

默认嵌入模型、分词资源、CPU 原生推理和必要的 VC DLL 均随 x64 包提供；用户不需要 Python、Docker 或另下载嵌入模型。首次构建需要下载固定权重，并需要对应版本且有许可的正式 Visual Studio Redist；这些是构建机依赖，不是用户运行依赖。当前未验收 Windows arm64 的 `sqlite-vec` 原生发布和正式安装器首用。

内置 Node 包固定为 Playwright MCP 0.0.83、Context7 MCP 4.1.1、Chrome DevTools MCP 1.10.1、Brave Search MCP 2.1.4 和 DBHub 1.4.0。Context7 默认仍走官方远程 HTTP，随包保留本地入口。Brave 仍需真实 API Key；DBHub 仍需用户明确的只读配置与数据库连接，特定数据库可能另需客户端。用户第三方 MCP 的依赖不会自动下载；Python/uvx 服务不因随附 Node 而自动可用。

`runtime/mcp/manifest.json` 是小型入口清单，`payload-integrity.json` 记录完整文件校验；原始 Node 许可证及依赖包内 LICENSE/NOTICE 随清单打包。浏览器使用与固定 Playwright 对应的 Chromium，不借用或启动用户日常浏览器。固定依赖只在构建机 `npm ci --ignore-scripts`，后续复用已验证载荷；`-Offline` 要求本地缓存已齐备。此运行时包含真实浏览器及数据库依赖，磁盘体积与进程驻留分别统计，不能将其体积称为 Rust 核心的内存。

在仓库根目录验证：

```powershell
dotnet build apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64 -p:EnableWinAppRunSupport=false
powershell -NoProfile -File apps/desktop/Build/prepare-native-runtime.ps1 -AssetsOnly -Offline
```

运行时检查复制本次构建的载荷到临时目录，清空系统 Node 路径并隐藏系统 .NET，仅使用隔离数据；`native-runtime-smoke/run.ps1` 核对主载荷，`check-mcp-runtime.ps1` 使用私有 Node 握手五个真实本地 MCP，数据库仅用内存演示夹具、Brave 仅用模拟 Key，Playwright 仅访问 about:blank。它们不调用真实模型，不替代正式 MSIX 安装、外部账号能力或其他架构设备的验收。

## 源码路径与调试符号

仓库根 `Directory.Build.props` 和 `Directory.Build.targets` 对 Debug、Release 都保留原有 PDB 和行号，将仓库源码根映射为 `/_/KYNXA/`，将 NuGet 包源码根映射为 `/_/NuGet/`。后者在包还原属性加载后求值，覆盖 Windows App SDK 自动加入的初始化源码。映射不修改运行时存储路径、不嵌入额外源码，也不会自动重写已生成的旧包；分发前须重新构建。

跨机器调试时，将两个虚拟根分别对应到本机仓库与 NuGet 缓存目录；调试器提示找不到源码时可选择实际源码文件。若本机调试需要保留绝对路径，可在仅本机的构建命令中传入 `-p:KynxaMapSourcePaths=false`，该输出不可用作发布包。PDB 映射仅处理符号中的源码文档名，不代表已经完成安装包、构建日志或原生依赖的完整隐私审核。
