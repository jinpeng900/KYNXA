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

运行时检查复制本次构建的载荷到临时目录，清空 PATH 并隐藏系统 .NET，仅使用隔离数据和禁用的默认 MCP 配置。它不替代正式 MSIX 安装或其他架构设备的验收。
