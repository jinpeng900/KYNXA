# KYNXA EXE 安装器

当前 Windows x64 安装发行版本为 **1.7.1**，发行入口为 [1.7.1 GitHub Release](https://github.com/jinpeng900/KYNXA/releases/tag/v1.7.1)，本批验证状态见 [1.7.1 更新说明](../../../../docs/releases/1.7.1.md)。**1.7** 保留为首个正式版的历史版本。源码工程与安装包使用对应版本；更新安装文件需要维护者重新生成载荷，并以匹配的发布标签记录源码。

**历史验证：** 1.5.3 的 Debug Full 构建、启动和 17 项打包守卫成绩记录在 [1.5.3 更新说明](../../../../docs/releases/1.5.3.md)，不作为当前 1.7.1 的验收成绩。该历史版本的安装 EXE 只构建，未执行安装测试；编译成功或应用启动不能代替安装验收。

基础资源直接随完整 EXE 提供，基础安装无需联网。可选模型下载由后台自动选择原站、国内镜像及已配置的 GitHub 资产来源，失败时自动切换并保留断点；安装界面不要求用户选择站点。来源清单保存在实际模型目录中，升级保留已有自定义映射和下载断点，完整机制见[离线安装与多来源下载](../../../../docs/architecture/offline-installer.md)。

普通用户下载 `KYNXA-1.7.1-windows-x64-setup.zip`，解压后运行其中的 `KYNXA-1.7.1-windows-x64-setup.exe`。本版公开附件仅安装包 ZIP 和资源 ZIP，不单独分发安装 EXE、在线入口或维护清单。

同版本压缩分发文件名为 `KYNXA-1.7.1-windows-x64-setup.zip` 与 `KYNXA-1.7.1-windows-x64-resources.zip`，分别用于安装器压缩分发和源码构建的运行资源准备。资源包保留 E5、Office 解析与转换、Chromium、私有 Node／MCP 及原生运行依赖；检索使用 E5 加词法 RRF 和轻量排序，不再携带 BGE。各文件大小与 SHA-256 以实际产物和对应清单为准，生成清单不代表已经上传。

`KYNXA.iss` 由发布入口使用 Inno Setup 6.4 或更新版本编译，只接收经过整理的 Windows x64 Release 暂存目录。安装器本身不依赖用户已有的 Node、Rust、.NET SDK、Python、Git 或编译工具。

## 编译输入

维护者传入以下 ISCC 定义；不把开发机绝对路径写入产品配置。

| 定义 | 用途 |
|---|---|
| `PayloadRoot` | 必填，已准备好的 Release 软件载荷 |
| `PackageOutputDir` | 输出目录，默认项目内 `artifacts/installer/output`，可传入独立分发目录 |
| `PackageVersion` | 安装发行版本，生成当前包时使用 `1.7.1` |
| `LauncherFile` | 相对软件根目录的启动器，默认 `KYNXA.Desktop.exe` |
| `IconFile`、`BrandImageFile` | 可选，安装器图标和项目 PNG 标识 |

生成文件名为 `KYNXA-<版本>-windows-x64-setup.exe`，使用 ZIP 压缩缩短完整资源的打包和解压等待。发布入口只生成文件，不自动执行安装器。

普通用户直接使用 Release 安装文件。下面的源码构建命令仅供已经准备开发环境的开发者使用，在仓库根目录运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Build -Configuration Release
```

该命令编译源码，不生成安装器。维护者在发行工作区使用 `apps/desktop/Build/build-installer.ps1` 等打包工具整理载荷。源码、Markdown 开发文档和固定依赖清单保存在 Git 中；本版通过 Release 分发安装包 ZIP 与运行资源 ZIP，安装 EXE 位于安装包 ZIP 内。历史设计归档不参与当前安装流程。

构建输出同时提供 `installer-release.json`、`Get-KynxaInstaller.ps1` 与共用 `RuntimeDownloads.psm1`，供安装包本身的多来源续传。下载脚本默认只准备文件，显式指定 `-Install` 才启动 EXE；直接运行已经下载完整的 EXE 不需要这些脚本。清单将未上传的 GitHub 地址标为 `pending`，不会当成已经存在的下载源。

在线入口工具保留供维护者使用，本版不分发在线入口。维护者另行提供完整 EXE 下载源并更新清单的可用来源后，可运行 `apps/desktop/Build/build-online-installer.ps1 -OutputDirectory <已有分发目录>` 生成在线 EXE。该入口复用完整包的清单，仅编译 `KYNXA.Online.iss` 与 `Start-KynxaOnline.ps1` 等小型下载载荷，不重编应用或完整包；不能把本版安装包 ZIP 当作其完整 EXE 来源。

`-ModelDownloadSourcesPath` 用于选择随产品分发的模型来源模板；`-DownloadSourcesPath` 用于构建机下载来源及发布资产配置。`installerAssets.<资产ID>.sources` 可追加原站或镜像 URL，`installerReleaseSources` 可声明安装包实际分发地址，均不改变固定版本、大小和 SHA-256。

载荷只包含软件运行文件、固定私有运行时、浏览器、E5 模型及许可证，不应含用户数据、开发缓存、测试文件、私有凭据或可选大型对话模型。`Data`、`Extensions`、开发 PDB 和 `prerequisites` 从软件通配复制中排除；WebView2 离线安装程序以临时先决组件单独打包。

## 安装界面和配置合同

单页提供项目图标、按物理内存推荐的可选 Qwen3 模型、软件/数据/工具目录、简中与英语，以及桌面快捷方式。可选模型默认不勾选，安装器展示下载大小；选择后由应用首次启动进行后台下载并显示进度，不把未下载的权重显示成已经安装。

默认安装为当前用户 `%LOCALAPPDATA%\Programs\KYNXA`。数据和工具默认放在软件目录下的 `Data`、`Extensions`；用户可以选择独立位置。修改软件目录时只更新仍为默认值的数据/工具路径。再次运行安装器读取上次目录、语言和快捷方式设置，已安装目录通过配置 helper 的只读入口解析迁移后实际数据/工具位置。

硬件推荐使用 Windows `GlobalMemoryStatusEx`，没有使用会截断大显存的 WMI `AdapterRAM`。推荐只决定预选项；用户可自行选择更大的模型，实际 GPU 后端、显存分配和空闲休眠由 Rust 资源服务决定。

安装结束隐藏调用：

```text
{app}\installer\Configure-Kynxa.ps1
    -ApplicationRoot <软件根目录>
    -DataRoot <数据根目录>
    -ExtensionRoot <用户工具目录>
    -Language zh-CN|en
    [-ModelId <目录中的模型 ID>]
```

该脚本由统一发布入口提供，负责保存统一路径与语言、初始化随包中英文检索资源、记录首次启动可选模型任务。EXE 载荷采用自包含 unpackaged WinUI，不要求用户安装开发证书或开发环境；原有 MSIX 开发链保持独立。返回 `0` 表示配置完成；非零使安装器显示失败，用户可查看 `{app}\installer\configuration.log` 后重新运行。路径通过单独命名参数传递，不生成执行字符串或修改系统 PATH。

同一 helper 的只读参数集为 `-ReadConfiguration -ApplicationRoot <已有目录> -OutputFile <临时 INI>`，输出 UTF-16 BOM INI 的 `[Kynxa]` 节，字段为 `DataRoot`、`ExtensionRoot`、`Language`。不存在旧配置时返回 `0` 且不输出文件；读取不写入用户数据。

WebView2 从注册表的 EdgeUpdate 客户端中识别，缺失时运行载荷的 `prerequisites\MicrosoftEdgeWebView2RuntimeInstallerX64.exe /silent /install`。安装包必须提供这一先决组件，Chromium 不能代替 WinUI 所需的 WebView2。

卸载先隐藏调用 `{app}\installer\Remove-Kynxa.ps1 -ApplicationRoot <软件根目录>` 停止本目录所属应用进程。安装器不递归删除软件根目录，也不删除任何 `Data`、`Extensions` 或外部模型目录。

## 风格

使用项目图标、中性浅色、紧凑布局、系统中英文字体，关闭输入框与按钮的主题蓝色边框并采用圆角裁切。保留原生键盘和辅助功能；不把 WinUI 和新的 GUI 运行时再套进安装器。安装器自有页面支持即时切换中英，通用 Windows 系统对话框遵循系统/安装器启动语言。
