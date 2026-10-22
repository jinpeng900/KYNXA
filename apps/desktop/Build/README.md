# 桌面运行时打包

适用版本：Windows x64 首个正式版 **KYNXA 1.7**（安装发行版本 `1.7.0`）。当前入口见 [文档索引](../../../docs/README.md) 和 [1.7 版本说明](../../../docs/releases/1.7.md)。修改版本配置不会自动重命名或更新旧安装 EXE。

Windows 包身份内部版本为 `1.70.0.0`，用于保持相对旧 `1.65.0.0` 的升级顺序；对外产品与安装版本使用 `1.7`／`1.7.0`。本版发布完整离线 EXE、安装器 ZIP、资源 ZIP 及在线安装入口；在线入口使用已发布并核验的完整包来源。

面向普通用户的 EXE 安装包使用 `build-installer.ps1 -OutputDirectory <输出目录>` 生成。此入口发布 Windows x64 Release 自包含应用，再编译 Inno Setup 安装器；它只生成文件，不启动或安装生成的 EXE。安装器支持软件、数据、用户工具目录及中英文界面，E5、私有 Node/MCP、Chromium 和运行依赖随基础包提供，勾选的对话模型交由首次启动后的 Rust 模型管理器下载。

默认安装文件名为 `KYNXA-1.7.0-windows-x64-setup.exe`，同版本运行资源文件名为 `KYNXA-1.7.0-windows-x64-resources.zip`。生成的 EXE、资源 ZIP 与本机模型保留在忽略的输出目录或独立分发目录；仓库只维护源码、Markdown 开发说明和依赖清单，旧设计压缩包与生成文档不再纳入 Git。

完整基础包不依赖安装时的外网连接；可选模型以及安装包本身的联网下载分别复用 Rust 模型管理器和 `RuntimeDownloads.psm1`，支持原站、明确发布的 GitHub 资产、国内镜像和独立来源断点。输出目录带有安装包下载入口及版本清单，详情见[离线安装与多来源下载](../../../docs/architecture/offline-installer.md)。

该发行入口使用 WinUI 的无包身份部署模式，原有开发包启动链保持不变。编译器只解压在项目缓存中，WebView2 使用经过 Microsoft 发布者签名验证的离线安装程序；用户无需安装 SDK、Node 或修改 PATH。当前入口尚未配置生产代码签名，不把开发证书导入用户信任库。升级保留数据和用户工具；卸载只移除安装文件并停止本安装的进程。

`KYNXA.Desktop.csproj` 定义桌面平台、依赖、共享合同和明确的发布资源清单；通过 `RuntimePackaging.targets` 接入独立执行载荷的构建流程。这里不读取用户的 Data、模型凭据或自定义扩展。

原生 HTTP/SSE 网关通过 `prepare-native-runtime.ps1` 构建 `runtime/agent/kynxa-gateway.exe`，资源服务为 `runtime/resource/kynxa-resource-service.exe`；Debug/Release 都验证源码指纹及资产 SHA-256。核心不依赖 Node、NAPI 或 JavaScript 网关；安装包在独立 `runtime/mcp` 目录随附固定私有 Node 和内置 MCP 依赖，WebView2 前端脚本保留。用户不需要安装 Node、配置系统 PATH 或运行 npx。源码构建需要 Rust 1.88+ 和 MSVC，现有 dotnet run 命令继续使用。

| 文件 | 职责 |
|---|---|
| `RuntimePackaging.targets` | 加入原生网关、资源服务、ToolHost、本地 E5 嵌入模型、私有 MCP 运行时、原始许可证及 PRI 处理 |
| `native-assets/*.json` | 固定 ORT 1.21.0、DirectML 1.15.4、sqlite-vec 0.1.9、模型与许可的原始下载地址、长度和 SHA-256 |
| `prepare-native-runtime.ps1` | PowerShell 5.1 构建/校验原生载荷，复用 Cargo 与模型缓存；下载归档只提取白名单 DLL/许可，不执行 SDK 安装代码 |
| `RuntimeDownloads.psm1` | 原生库、模型与 Node 的共用下载：保留 `.part`，核对范围和文件身份后续传，有限重试及配置镜像回退，显示大小与速度，完成后校验固定 SHA-256 |
| `prepare-mcp-runtime.ps1` / `native-assets/mcp-runtime.json` | 构建机校验官方 Node 24.14.0 ZIP 和执行文件 SHA-256，按固定 package-lock 安装内置 MCP 与对应 Chromium；不运行依赖安装脚本，不随包携带 npm/npx |
| `prepare-toolhost-notices.ps1` | 从实际恢复的 .NET 运行时包复制原许可证和第三方声明 |
| `prepare-resource-pack.ps1` | 从已准备的发布白名单导出固定资源 ZIP；导入时核对当前版本清单、文件长度与 SHA-256，不包含聊天、凭据及开发依赖缓存 |
| `build-desktop.ps1` | 统一 Setup、Build、Run、Rebuild 与 Publish；环境预检、阶段续接、依赖复用与直接启动已有产物 |
| `DevelopmentEnvironment.psm1` | 发现正式版/Insiders VS、有效 Windows SDK、稳定 .NET 10 与兼容 Rust；仅调整调用进程的编译环境 |
| `DesktopBuild.psm1` / `LaunchDesktop.ps1` | 构建状态、日志、增量判定和带包身份启动；不执行模型任务或修改正式数据 |
| `vc-runtime.json` / `prepare-vc-runtime.ps1` | 从固定正式 Microsoft Redist 获取并验证 x64 app-local VC 运行库及许可；生成明确发布清单，不复制 System32 DLL |
| `filter-runtime-pri-layout.ps1` | 将独立运行时保留为普通包载荷，不让 WinUI 把它们解释成界面资源 |
| `remove-obsolete-backend-payload.ps1` | 仅在验证后的桌面输出中移除精确旧 Node/NAPI/网关载荷；正式数据和链接路径拒绝清理 |
| `check-deployment-paths.ps1` | 构建时预检原生载荷在当前 `LayoutDir` 中的最长路径；超限给出具体路径和短目录设置建议，不自动移动或删除目录 |

所有项目路径以 `MSBuildProjectDirectory` 和项目内相对路径解析，输出缓存位于项目的中间目录或忽略的 `artifacts/runtime`。不根据开发机用户名、盘符或安装 SDK 的猜测版本定位运行时。嵌入权重和 VC 文件仅按验证后的发布清单复制，缓存杂项不进入安装包。模型资产清单位于 `Build/native-assets`；官方只读资料位于 `apps/official-tools`。实际发布根据 bundle-files.txt 白名单，不将下载归档、旧 Node 目录或缓存杂项打包。

默认 E5 嵌入模型、分词资源、CPU 原生推理和必要的 VC DLL 均随 x64 包提供；用户不需要 Python、Docker 或另下载嵌入模型。检索采用 E5 加词法 RRF 与轻量排序，BGE 权重及专属清单不再进入安装器或资源 ZIP，已有 E5 索引继续使用。首次构建需要下载固定权重，并需要对应版本且有许可的正式 Visual Studio Redist；这些是构建机依赖，不是用户运行依赖。Windows arm64 不在本版发行范围，安装器首用验收须与构建和启动结果分别记录。

内置 Node 包固定为 Playwright MCP 0.0.83、Context7 MCP 4.1.1、Chrome DevTools MCP 1.10.1、Brave Search MCP 2.1.4 和 DBHub 1.4.0。Context7 默认仍走官方远程 HTTP，随包保留本地入口。Brave 仍需真实 API Key；DBHub 仍需用户明确的只读配置与数据库连接，特定数据库可能另需客户端。用户第三方 MCP 的依赖不会自动下载；Python/uvx 服务不因随附 Node 而自动可用。

`runtime/mcp/manifest.json` 是小型入口清单，`payload-integrity.json` 记录完整文件校验；原始 Node 许可证及依赖包内 LICENSE/NOTICE 随清单打包。浏览器使用与固定 Playwright 对应的 Chromium，不借用或启动用户日常浏览器。固定依赖只在构建机 `npm ci --ignore-scripts`，后续复用已验证载荷；`-Offline` 要求本地缓存已齐备。此运行时包含真实浏览器及数据库依赖，磁盘体积与进程驻留分别统计，不能将其体积称为 Rust 核心的内存。

首次安装按清单验证固定 Node、Chromium、模型和服务入口的版本、长度及 SHA-256，完整成功后保存组件就绪记录。再次运行先批量检查输入、文件元信息及目录变化，未变的组件跳过解压和逐文件哈希，变化部分按原始清单重新核验；需要主动完整复核时使用 `-VerifyResources`。配置文本的 LF/CRLF/BOM 等价规则继续保留，不放宽第三方二进制与依赖锁的原始字节校验。清单原子发布，失败不写就绪状态；构建锁串行保护同一项目，调用者的编译环境在退出时恢复。

## 一个入口准备、构建与运行

在仓库根目录执行。开发机需要 Windows x64、稳定版 .NET 10，以及包含 x64 C++ 工具和 Windows SDK 的 VS；脚本同时发现正式版和 Insiders，汇总需手动安装的缺项。兼容的已有 Rust 优先复用，缺失时在项目内补齐固定工具链。`global.json` 避免选择机器上额外安装的 .NET 11 预览版。安装成品软件的用户不需要这些源码构建步骤。

```powershell
# 首次使用自动检查、准备、构建并启动；以后相同命令直接复用成功产物。
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Run

# 只准备工具链、资源和 NuGet 依赖，不编译。
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Setup

# 增量构建；明确要求时重新执行构建，保留下载和依赖缓存。
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Build
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Rebuild
```

项目根目录中只有一份 `kynxa-*-resources*.zip` 时自动选用，也可显式指定 `-ResourcePackRoot <ZIP完整路径>`。`-Offline` 禁止资源与 Cargo 联网；NuGet 使用已有有效恢复结果或明确的本地源。原有 `-Configuration`、`-Profile`、下载源与 NuGet 选项保留，`Publish` 继续可用。

状态与日志固定在 `artifacts/development/build/win-x64/<Debug或Release>/<Full或Core>/`，不同配置分别管理。同一项目同时只运行一份准备或构建。每阶段完成后保存状态；失败或中断不会删除已下载的工具链、资源和依赖。再次运行校验已完成步骤后接续，编译仍交给 Cargo/MSBuild 增量处理。`Run` 在输入和产物仍匹配时不再调用 `dotnet run` 或 MSBuild 的运行目标，避免重新触发整套准备流程。

在仓库根目录验证：

```powershell
dotnet build apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64 -p:EnableWinAppRunSupport=false
powershell -NoProfile -File apps/desktop/Build/prepare-native-runtime.ps1 -AssetsOnly -Offline
```

## 更新源码与随包浏览器

完整 `Full` 构建把 Chromium 放在应用目录的 `runtime/mcp/browsers/`，私有 Node 和内置 MCP 也随包准备；最终用户不需要另外安装 Chrome、Node 或配置 PATH。浏览器、模型权重等大文件通过运行资源或发布资源包分发，Git 源码仓库只保存版本清单、实现和准备脚本。

已有开发环境的成员拉取更新后，执行默认 `Full` 构建或 `build-desktop.ps1 -Action Run`，脚本会复用已有资源并补齐缺少部分。有本地资源 ZIP 时，使用 `-ResourcePackRoot <资源ZIP完整路径>`；完全离线还需要对应 SDK、NuGet/Cargo 缓存及完整运行资源。`Core` 只用于开发编译，不能当成包含浏览器与模型的完整成品。

默认部署目录为 `artifacts/development/appx/x64/Debug`，无参数网关及本地模型启动脚本优先读取此目录，并兼容原来的 `bin/.../AppX` 输出。显式指定应用目录仍按调用参数执行。

## 固定资源包与离线源码构建

维护者先按默认 `Full` 准备一次完整资源，再从 `artifacts/runtime/native-desktop/win-x64/asset-files.txt` 导出；该清单缺失时使用 `bundle-files.txt`。导出会加入现有 VC 缓存的明确 DLL 与许可证白名单，不加入下载归档、Rust `target`、Cargo/NuGet 缓存、用户 Data 或模型账号。ZIP 旁生成整包 `.sha256`，包内 `resource-pack.json` 记录每份文件摘要和当前原生库、E5、MCP 锁定清单的版本摘要。

```powershell
# 维护者：只处理已准备好的本地资源，不进行下载或编译。
powershell -NoProfile -File apps/desktop/Build/prepare-resource-pack.ps1 -Action Export -ArchivePath artifacts/runtime/kynxa-win-x64-resources.zip

# 接收者：直接传 ZIP；构建入口负责核对并导入资源缓存。
powershell -NoProfile -File apps/desktop/Build/build-desktop.ps1 -ResourcePackRoot artifacts/runtime/kynxa-win-x64-resources.zip
```

默认 `Full` 包含检索模型、原生依赖、私有 Node/MCP 和固定 Chromium。`-Profile Core` 是开发预览档，只准备网关与直接检索所需的核心资源，不作为完整用户发行包；以后切回 `Full` 时补齐其余资源。原有启动命令继续可用，MSBuild 对应属性为 `KynxaRuntimeProfile`、`KynxaResourcePackRoot`、`KynxaDownloadSourcesPath`、`KynxaOffline`。

直接打开 `KYNXA.Desktop.slnx` 时，VS 会读取项目旁的 `KYNXA.Desktop.csproj.user`。仅准备了 Core 的开发机可在该文件中为 VS 的 Debug/x64 设置 `KynxaRuntimeProfile=Core`；此文件已被 Git 忽略，不把个人配置提交给其他成员。资源准备参数在构建目标执行时计算，采用用户文件导入后的最终值；工程默认值和 Release 仍为 Full。

如需复用本机 Cargo 缓存，可在同一用户文件中设置 `KynxaNativeBuildEnvironment`，格式为分号分隔的 `名称=值`（例如 `CARGO_HOME`、`CARGO_TARGET_DIR`）。该可选配置只传给原生资源准备的子进程，不修改 VS 宿主或系统环境；包含分号的单个值需要使用 MSBuild 转义。个人工具和缓存路径只放在本机用户文件中。

离线源码构建同时需要本机 .NET/Windows SDK、Rust/MSVC 工具链、对应 Cargo/NuGet 包缓存及还原结果。资源包解决用户运行资源分发，不包含这些构建机依赖。使用下列入口时，外部资源禁止联网，Cargo 使用 `--offline`，桌面使用 `--no-restore`，ToolHost 子构建只运行 `Build`：

```powershell
powershell -NoProfile -File apps/desktop/Build/build-desktop.ps1 -Offline -ResourcePackRoot artifacts/runtime/kynxa-win-x64-resources.zip
# 也可直接沿用 dotnet；--no-restore 与 KynxaOffline 必须同时传入。
dotnet build apps/desktop/KYNXA.Desktop.csproj --no-restore -p:Platform=x64 -p:KynxaOffline=true -p:KynxaResourcePackRoot=artifacts/runtime/kynxa-win-x64-resources.zip
```

`-NuGetSource` 可指定 NuGet 源，`-NuGetConfigPath` 可指定明确的 NuGet 配置。离线时只接受本地源；有本地源时先从该源还原桌面和 ToolHost，随后使用 `--no-restore` 构建。未指定本地源时使用正常 NuGet 包缓存重新解析必要依赖，缺少的包会报告还原错误，不自动切回网络下载。

```powershell
powershell -NoProfile -File apps/desktop/Build/build-desktop.ps1 -Offline -NuGetSource artifacts/dependencies/nuget-feed -ResourcePackRoot artifacts/runtime/kynxa-win-x64-resources.zip
```

下载源通过 `-DownloadSourcesPath` 或 MSBuild `KynxaDownloadSourcesPath` 配置。源文件可分别设置 `npmRegistry`、`cargoRegistryIndex`、`playwrightDownloadHost` 与 `mirrors`（原始 URL 前缀到可信镜像前缀的映射）。镜像只改变下载地址，版本及原始 SHA-256 保持不变；不设置时继续使用官方源。Cargo 离线规则参见 [Cargo build](https://doc.rust-lang.org/cargo/commands/cargo-build.html)，浏览器下载服务器参见 [Playwright 官方说明](https://playwright.dev/docs/browsers#download-from-artifact-repository)。

将 `download-sources.example.json` 复制到自己的配置位置，填入已选定的镜像，再通过 `-DownloadSourcesPath` 传入；不改用户全局 npm/Cargo 配置。原生库、E5、Node 与 VC 许可下载会保留断点；Chromium 继续使用 Playwright 自带的下载重试及服务器配置，完整资源包可以直接避开这一步下载。DirectML 与 ORT 的原始许可及来源已随源码和资源包提供，不再下载只用于提取许可的两份 NuGet 包。

运行时检查复制本次构建的载荷到临时目录，清空系统 Node 路径并隐藏系统 .NET，仅使用隔离数据；`native-runtime-smoke/run.ps1` 核对主载荷，`check-mcp-runtime.ps1` 使用私有 Node 握手五个真实本地 MCP，数据库仅用内存演示夹具、Brave 仅用模拟 Key，Playwright 仅访问 about:blank。它们不调用真实模型，不替代正式 MSIX 安装、外部账号能力或其他架构设备的验收。

## 源码路径与调试符号

仓库根 `Directory.Build.props` 和 `Directory.Build.targets` 对 Debug、Release 都保留原有 PDB 和行号，将仓库源码根映射为 `/_/KYNXA/`，将 NuGet 包源码根映射为 `/_/NuGet/`。后者在包还原属性加载后求值，覆盖 Windows App SDK 自动加入的初始化源码。映射不修改运行时存储路径、不嵌入额外源码，也不会自动重写已生成的旧包；分发前须重新构建。

跨机器调试时，将两个虚拟根分别对应到本机仓库与 NuGet 缓存目录；调试器提示找不到源码时可选择实际源码文件。若本机调试需要保留绝对路径，可在仅本机的构建命令中传入 `-p:KynxaMapSourcePaths=false`，该输出不可用作发布包。PDB 映射仅处理符号中的源码文档名，不代表已经完成安装包、构建日志或原生依赖的完整隐私审核。
