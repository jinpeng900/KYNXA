# 桌面运行时打包

`KYNXA.Desktop.csproj` 定义桌面平台、依赖、共享合同和明确的发布资源清单；通过 `RuntimePackaging.targets` 接入独立执行载荷的构建流程。这里不读取用户的 Data、模型凭据或自定义扩展。

原生 HTTP/SSE 网关通过 `prepare-native-runtime.ps1` 构建 `runtime/agent/kynxa-gateway.exe`，资源服务为 `runtime/resource/kynxa-resource-service.exe`；Debug/Release 都验证源码指纹及资产 SHA-256。核心不依赖 Node、NAPI 或 JavaScript 网关；安装包在独立 `runtime/mcp` 目录随附固定私有 Node 和内置 MCP 依赖，WebView2 前端脚本保留。用户不需要安装 Node、配置系统 PATH 或运行 npx。源码构建需要 Rust 1.88+ 和 MSVC，现有 dotnet run 命令继续使用。

| 文件 | 职责 |
|---|---|
| `RuntimePackaging.targets` | 加入原生网关、资源服务、ToolHost、本地嵌入与重排模型、私有 MCP 运行时、原始许可证及 PRI 处理 |
| `native-assets/*.json` | 固定 ORT 1.21.0、DirectML 1.15.4、sqlite-vec 0.1.9、模型与许可的原始下载地址、长度和 SHA-256 |
| `prepare-native-runtime.ps1` | PowerShell 5.1 构建/校验原生载荷，复用 Cargo 与模型缓存；下载归档只提取白名单 DLL/许可，不执行 SDK 安装代码 |
| `RuntimeDownloads.psm1` | 原生库、模型与 Node 的共用下载：保留 `.part`，核对范围和文件身份后续传，有限重试及配置镜像回退，显示大小与速度，完成后校验固定 SHA-256 |
| `prepare-mcp-runtime.ps1` / `native-assets/mcp-runtime.json` | 构建机校验官方 Node 24.14.0 ZIP 和执行文件 SHA-256，按固定 package-lock 安装内置 MCP 与对应 Chromium；不运行依赖安装脚本，不随包携带 npm/npx |
| `prepare-toolhost-notices.ps1` | 从实际恢复的 .NET 运行时包复制原许可证和第三方声明 |
| `prepare-resource-pack.ps1` | 从已准备的发布白名单导出固定资源 ZIP；导入时核对当前版本清单、文件长度与 SHA-256，不包含聊天、凭据及开发依赖缓存 |
| `build-desktop.ps1` | 将离线、资源包、下载源及 NuGet 配置贯通到现有桌面构建；不替代原有 `dotnet build/run` 入口 |
| `vc-runtime.json` / `prepare-vc-runtime.ps1` | 从固定正式 Microsoft Redist 获取并验证 x64 app-local VC 运行库及许可；生成明确发布清单，不复制 System32 DLL |
| `filter-runtime-pri-layout.ps1` | 将独立运行时保留为普通包载荷，不让 WinUI 把它们解释成界面资源 |
| `remove-obsolete-backend-payload.ps1` | 仅在验证后的桌面输出中移除精确旧 Node/NAPI/网关载荷；正式数据和链接路径拒绝清理 |
| `check-deployment-paths.ps1` | 构建时预检原生载荷在当前 `LayoutDir` 中的最长路径；超限给出具体路径和短目录设置建议，不自动移动或删除目录 |

所有项目路径以 `MSBuildProjectDirectory` 和项目内相对路径解析，输出缓存位于项目的中间目录或忽略的 `artifacts/runtime`。不根据开发机用户名、盘符或安装 SDK 的猜测版本定位运行时。嵌入权重和 VC 文件仅按验证后的发布清单复制，缓存杂项不进入安装包。模型资产清单位于 `Build/native-assets`；官方只读资料位于 `apps/official-tools`。实际发布根据 bundle-files.txt 白名单，不将下载归档、旧 Node 目录或缓存杂项打包。

默认嵌入模型、分词资源、CPU 原生推理和必要的 VC DLL 均随 x64 包提供；用户不需要 Python、Docker 或另下载嵌入模型。首次构建需要下载固定权重，并需要对应版本且有许可的正式 Visual Studio Redist；这些是构建机依赖，不是用户运行依赖。当前未验收 Windows arm64 的 `sqlite-vec` 原生发布和正式安装器首用。

内置 Node 包固定为 Playwright MCP 0.0.83、Context7 MCP 4.1.1、Chrome DevTools MCP 1.10.1、Brave Search MCP 2.1.4 和 DBHub 1.4.0。Context7 默认仍走官方远程 HTTP，随包保留本地入口。Brave 仍需真实 API Key；DBHub 仍需用户明确的只读配置与数据库连接，特定数据库可能另需客户端。用户第三方 MCP 的依赖不会自动下载；Python/uvx 服务不因随附 Node 而自动可用。

`runtime/mcp/manifest.json` 是小型入口清单，`payload-integrity.json` 记录完整文件校验；原始 Node 许可证及依赖包内 LICENSE/NOTICE 随清单打包。浏览器使用与固定 Playwright 对应的 Chromium，不借用或启动用户日常浏览器。固定依赖只在构建机 `npm ci --ignore-scripts`，后续复用已验证载荷；`-Offline` 要求本地缓存已齐备。此运行时包含真实浏览器及数据库依赖，磁盘体积与进程驻留分别统计，不能将其体积称为 Rust 核心的内存。

复用缓存前同时验证清单 schema、非空且唯一的安全路径、文件数与总字节数、固定 Node/Chromium/服务入口，以及每个文件的实际长度和 SHA-256。配置文本的 LF/CRLF/BOM 等价规则继续保留，不放宽第三方二进制与依赖锁的原始字节校验。清单原子发布；Debug/Release 的原生载荷准备由同一缓存锁串行保护，调用者的编译环境在退出时恢复。

在仓库根目录验证：

```powershell
dotnet build apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64 -p:EnableWinAppRunSupport=false
powershell -NoProfile -File apps/desktop/Build/prepare-native-runtime.ps1 -AssetsOnly -Offline
```

## 固定资源包与离线源码构建

维护者先按默认 `Full` 准备一次完整资源，再从 `artifacts/runtime/native-desktop/win-x64/asset-files.txt` 导出；该清单缺失时使用 `bundle-files.txt`。导出会加入现有 VC 缓存的明确 DLL 与许可证白名单，不加入下载归档、Rust `target`、Cargo/NuGet 缓存、用户 Data 或模型账号。ZIP 旁生成整包 `.sha256`，包内 `resource-pack.json` 记录每份文件摘要和当前原生库、E5、BGE、MCP 锁定清单的版本摘要。

```powershell
# 维护者：只处理已准备好的本地资源，不进行下载或编译。
powershell -NoProfile -File apps/desktop/Build/prepare-resource-pack.ps1 -Action Export -ArchivePath artifacts/runtime/kynxa-win-x64-resources.zip

# 接收者：明确选择解压目录；导入后构建优先复用本地固定资源。
powershell -NoProfile -File apps/desktop/Build/prepare-resource-pack.ps1 -Action Import -ArchivePath artifacts/runtime/kynxa-win-x64-resources.zip -DestinationRoot artifacts/runtime/resource-pack/win-x64
powershell -NoProfile -File apps/desktop/Build/build-desktop.ps1 -ResourcePackRoot artifacts/runtime/resource-pack/win-x64
# 也可直接传 ZIP；构建入口负责核对并导入资源缓存。
powershell -NoProfile -File apps/desktop/Build/build-desktop.ps1 -ResourcePackRoot artifacts/runtime/kynxa-win-x64-resources.zip
```

默认 `Full` 包含检索模型、原生依赖、私有 Node/MCP 和固定 Chromium。`-Profile Core` 是开发预览档，只准备网关与直接检索所需的核心资源，不作为完整用户发行包；以后切回 `Full` 时补齐其余资源。原有启动命令继续可用，MSBuild 对应属性为 `KynxaRuntimeProfile`、`KynxaResourcePackRoot`、`KynxaDownloadSourcesPath`、`KynxaOffline`。

离线源码构建同时需要本机 .NET/Windows SDK、Rust/MSVC 工具链、对应 Cargo/NuGet 包缓存及还原结果。资源包解决用户运行资源分发，不包含这些构建机依赖。使用下列入口时，外部资源禁止联网，Cargo 使用 `--offline`，桌面使用 `--no-restore`，ToolHost 子构建只运行 `Build`：

```powershell
powershell -NoProfile -File apps/desktop/Build/build-desktop.ps1 -Offline -ResourcePackRoot artifacts/runtime/resource-pack/win-x64
# 也可直接沿用 dotnet；--no-restore 与 KynxaOffline 必须同时传入。
dotnet build apps/desktop/KYNXA.Desktop.csproj --no-restore -p:Platform=x64 -p:KynxaOffline=true -p:KynxaResourcePackRoot=artifacts/runtime/resource-pack/win-x64
```

`-NuGetSource` 可指定 NuGet 源，`-NuGetConfigPath` 可指定明确的 NuGet 配置。离线时只接受本地源；有本地源时先从该源还原桌面和 ToolHost，随后使用 `--no-restore` 构建。没有本地源且没有已有 `project.assets.json` 时直接提示先还原对应项目，不自动改回网络下载。

```powershell
powershell -NoProfile -File apps/desktop/Build/build-desktop.ps1 -Offline -NuGetSource artifacts/dependencies/nuget-feed -ResourcePackRoot artifacts/runtime/resource-pack/win-x64
```

下载源通过 `-DownloadSourcesPath` 或 MSBuild `KynxaDownloadSourcesPath` 配置。源文件可分别设置 `npmRegistry`、`cargoRegistryIndex`、`playwrightDownloadHost` 与 `mirrors`（原始 URL 前缀到可信镜像前缀的映射）。镜像只改变下载地址，版本及原始 SHA-256 保持不变；不设置时继续使用官方源。Cargo 离线规则参见 [Cargo build](https://doc.rust-lang.org/cargo/commands/cargo-build.html)，浏览器下载服务器参见 [Playwright 官方说明](https://playwright.dev/docs/browsers#download-from-artifact-repository)。

将 `download-sources.example.json` 复制到自己的配置位置，填入已选定的镜像，再通过 `-DownloadSourcesPath` 传入；不改用户全局 npm/Cargo 配置。原生库、E5/BGE、Node 与 VC 许可下载会保留断点；Chromium 继续使用 Playwright 自带的下载重试及服务器配置，完整资源包可以直接避开这一步下载。DirectML 与 ORT 的原始许可及来源已随源码和资源包提供，不再下载只用于提取许可的两份 NuGet 包。

运行时检查复制本次构建的载荷到临时目录，清空系统 Node 路径并隐藏系统 .NET，仅使用隔离数据；`native-runtime-smoke/run.ps1` 核对主载荷，`check-mcp-runtime.ps1` 使用私有 Node 握手五个真实本地 MCP，数据库仅用内存演示夹具、Brave 仅用模拟 Key，Playwright 仅访问 about:blank。它们不调用真实模型，不替代正式 MSIX 安装、外部账号能力或其他架构设备的验收。

## 源码路径与调试符号

仓库根 `Directory.Build.props` 和 `Directory.Build.targets` 对 Debug、Release 都保留原有 PDB 和行号，将仓库源码根映射为 `/_/KYNXA/`，将 NuGet 包源码根映射为 `/_/NuGet/`。后者在包还原属性加载后求值，覆盖 Windows App SDK 自动加入的初始化源码。映射不修改运行时存储路径、不嵌入额外源码，也不会自动重写已生成的旧包；分发前须重新构建。

跨机器调试时，将两个虚拟根分别对应到本机仓库与 NuGet 缓存目录；调试器提示找不到源码时可选择实际源码文件。若本机调试需要保留绝对路径，可在仅本机的构建命令中传入 `-p:KynxaMapSourcePaths=false`，该输出不可用作发布包。PDB 映射仅处理符号中的源码文档名，不代表已经完成安装包、构建日志或原生依赖的完整隐私审核。
