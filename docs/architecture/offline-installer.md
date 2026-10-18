# KYNXA 1.5 离线安装与多来源下载

当前完整安装文件为 `KYNXA-1.5.3-windows-x64-setup.exe`，从 [GitHub Release 附件](https://github.com/jinpeng900/KYNXA/releases/tag/v1.5.3)获取。安装程序包含桌面、自包含 .NET/WinUI、原生网关与资源服务、文档解析组件、私有 Node/MCP、Chromium、E5/BGE 以及 WebView2 离线先决组件。基础安装读取本地载荷，不要求用户安装 SDK，也不因模型网站不可用而中断。

1.5.3 的构建、运行与打包验证范围见 [版本说明](../releases/1.5.3.md#验证范围与边界)。安装 EXE 仅构建，未执行安装测试；应用启动验证不替代安装验收。

可选对话模型不默认打入基础包。用户勾选后，软件首次启动把下载交给原有 Rust 模型管理器，界面和 API 聊天可以先使用。未勾选时不下载对话模型。

## 在线版与离线版

`KYNXA-1.5.3-windows-x64-online-setup.exe` 是小型在线入口：显示实际下载进度，自动下载完整安装包，完成校验后打开同一套安装界面。关闭下载窗口会保留断点；再次运行继续下载。用户不需要选择来源或运行 PowerShell 命令。

下载缓存优先放在在线 EXE 旁的专用文件夹；该目录不可写时使用当前用户的安装下载缓存。同目录已经存在完整离线安装包时直接复用。在线下载阶段不创建软件安装记录；正式安装仍由完整安装器处理。

在线版只减少首次拿到的文件大小，仍需下载约 1.35 GiB 的完整载荷。需要在无网络电脑安装时，提前保存完整离线 EXE。

## 两条下载路径

| 内容 | 下载与恢复职责 |
| --- | --- |
| 完整 EXE 本身 | 分发目录中的 `Get-KynxaInstaller.ps1` 根据 `installer-release.json` 下载或复用已存在文件；共用 `RuntimeDownloads.psm1`。默认只下载，显式传 `-Install` 才启动安装器。 |
| 可选 GGUF 模型 | Rust `gateway_local_download.rs` 按固定模型清单下载、暂停、恢复并发布文件；选择和进度仍在本地模型设置中。 |

已经完整下载的 EXE 可以直接离线安装。EXE 在开始运行之前，无法自行恢复此前未下载完整的自身文件，因此下载入口与安装本体分别承担这两个阶段。

## 下载来源

下载来源由软件自动处理，安装界面不要求用户选择或切换站点。后台在原始站点、国内镜像及明确发布的 GitHub 资产之间回退；失败时保留断点并尝试下一可用来源。原始模型地址保留固定 revision，国内镜像复用相同路径，所有来源仍需匹配同一模型的长度与 SHA-256。

随包 `installer/model-download-sources.json` 是维护者提供的初始配置。安装后保存于实际 `ModelHome/download-sources.json`，默认位置为用户选择的数据目录下的 `Models`。普通安装使用自动模式，重跑安装器保留已有自定义资源映射；改变数据目录后的指针解析继续复用统一存储路径。用户无需编辑配置。

```json
{
  "schemaVersion": 1,
  "preferred": "auto",
  "mirrors": {
    "https://huggingface.co/": ["https://hf-mirror.com/"]
  },
  "assets": {}
}
```

发行包使用 `preferred: auto`；其余偏好值仅供维护者诊断及兼容已有配置，不是普通用户的安装步骤。`assets` 使用完整 SHA-256 作为键，值为相同文件的明确 GitHub Release URL 数组；只有已经存在的同字节资源才应写入。模板没有伪造的模型附件地址。HF-Mirror 的域名与路径用法见[镜像站说明](https://hf-mirror.com/)。

GitHub 上的软件安装包和运行资源使用 Release 附件分发；源码仓库只保存实现、依赖清单和 Markdown 文档。GitHub 单个 Release 附件需小于 2 GiB，大于此限制的模型继续走原始模型站或国内镜像，不自动构造不可用的 GitHub 地址，详见[GitHub Release 说明](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases)。构建输出的待发布 URL 不代表已经上传。

## 中断、切换与重试

未完成下载保留 `.part` 和文件身份记录，重新运行继续可用断点。来源各自的有效断点保留，切换失败来源不清掉另一来源的有效部分。支持范围请求时校对 `206` 的起点、长度和文件身份，并使用 `If-Range`；服务器返回完整 `200` 时重写当前来源的文件，不把完整正文追加到旧断点。

完成后校验预期长度与固定 SHA-256，再将临时文件发布为正式文件。错误页面、错误范围、断流和坏文件不会标成已安装。连接失败按有限次数重试与来源回退，仍失败时保留进度，用户可以继续或重试。HTTP 范围与验证器语义见 [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html#name-range-requests)。

安装阶段中断后可以重新运行同一完整 EXE，软件文件重新复制，用户数据、工具和有效模型继续保留。这是可恢复的重新安装，不承诺从上次复制的单个文件字节继续；基础资源已经在本地，不需要再次下载。

## 维护者构建

普通用户使用 Release 附件，不需要构建。开发者已准备源码开发环境时，可在仓库根目录进行 Release 构建：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File apps/desktop/Build/build-desktop.ps1 -Action Build -Configuration Release
```

该命令只编译源码，不生成安装器；`-Action Run` 用于构建并启动。维护者在发行工作区使用 `apps/desktop/Build/build-installer.ps1` 生成安装器，当前包版本参数为 `1.5.3`。

发行脚本的 `-ModelDownloadSourcesPath` 选择发布给用户的模型来源配置，`-DownloadSourcesPath` 配置构建机资源来源；两者分开，避免把构建机配置或认证带进产品。源码构建入口的 `-Offline` 需要已准备对应 SDK 和依赖缓存，完整离线 EXE 的用户安装无需此参数。

完整包发布并把 `installer-release.json` 中的下载来源标记为 `available` 后，维护者可在发行工作区使用 `apps/desktop/Build/build-online-installer.ps1`，以版本 `1.5.3` 单独生成在线入口。

此入口只打包下载窗口、下载模块和现有发布清单，不重编 Rust、C# 或完整离线包。正常构建拒绝全部来源仍为 `pending` 的清单；维护者的 `-AllowUnpublished` 仅用于编译未发布模板，构建记录会标明不可直接下载。

软件目录、数据目录、用户工具目录、界面语言、快捷方式与硬件推荐模型继续由现有安装界面配置，见[安装器说明](../../apps/desktop/Build/Installer/README.md)。生成文件不会自动执行安装器、提交源码或发布 GitHub Release。

## 本次必要验证

2026-10-16：模型下载的 5 项本机 HTTP 回归、原有模型导入／启停／迁移／恢复回归 1 项、共用安装包下载夹具 6 项通过。覆盖真实断流、持久断点与 If-Range、故障来源回退、忽略或错误范围、SHA-256 失败不发布，以及成功后释放重复断点。安装配置往返验证了迁移后的模型目录、自定义映射和既有片段保留。

固定 Qwen3 0.6B 模型的原站和 HF-Mirror 各进行一次 1 字节范围请求，均返回 `206` 与相同总长度；没有据此宣称所有镜像或全部大模型都完成整文件下载。PowerShell 5.1 解析、ISCC 静态编译和架构检查通过。本轮不运行安装器。
