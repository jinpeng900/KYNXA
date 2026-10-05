# 桌面运行时打包

`KYNXA.Desktop.csproj` 定义桌面平台、依赖、共享合同和明确的发布资源清单；通过 `RuntimePackaging.targets` 接入独立执行载荷的构建流程。这里不读取用户的 Data、模型凭据或自定义扩展。

| 文件 | 职责 |
|---|---|
| `RuntimePackaging.targets` | 恢复网关依赖、加入 Node 和 ToolHost 载荷、接入许可证及 PRI 处理；保留原构建顺序和增量恢复条件 |
| `node-runtime.json` | 按平台固定官方 Node 版本和 SHA-256 |
| `prepare-node-runtime.ps1` | 并发锁、下载/缓存校验、逐文件提取和运行时完整性检查 |
| `prepare-toolhost-notices.ps1` | 从实际恢复的 .NET 运行时包复制原许可证和第三方声明 |
| `filter-runtime-pri-layout.ps1` | 将独立运行时保留为普通包载荷，不让 WinUI 把它们解释成界面资源 |

所有项目路径以 `MSBuildProjectDirectory` 和项目内相对路径解析，输出缓存位于项目的中间目录。不根据开发机用户名、盘符或安装 SDK 的猜测版本定位运行时。

在仓库根目录验证：

```powershell
dotnet build apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64 -p:EnableWinAppRunSupport=false
powershell -File tests/node-runtime-smoke/check-pri.ps1
powershell -File tests/node-runtime-smoke/run.ps1 -BundleRoot 'apps/desktop/bin/x64/Debug/net10.0-windows10.0.26100.0/win-x64'
```

运行时检查复制本次构建的载荷到临时目录，清空 PATH 并隐藏系统 .NET，仅使用隔离数据；检查默认启用的官方 MCP 清单时不发起连接、不启动上游服务。它不替代正式 MSIX 安装或其他架构设备的验收。

## 源码路径与调试符号

仓库根 `Directory.Build.props` 和 `Directory.Build.targets` 对 Debug、Release 都保留原有 PDB 和行号，将仓库源码根映射为 `/_/KYNXA/`，将 NuGet 包源码根映射为 `/_/NuGet/`。后者在包还原属性加载后求值，覆盖 Windows App SDK 自动加入的初始化源码。映射不修改运行时存储路径、不嵌入额外源码，也不会自动重写已生成的旧包；分发前须重新构建。

跨机器调试时，将两个虚拟根分别对应到本机仓库与 NuGet 缓存目录；调试器提示找不到源码时可选择实际源码文件。若本机调试需要保留绝对路径，可在仅本机的构建命令中传入 `-p:KynxaMapSourcePaths=false`，该输出不可用作发布包。PDB 映射仅处理符号中的源码文档名，不代表已经完成安装包、构建日志或原生依赖的完整隐私审核。
