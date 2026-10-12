# 原生 Windows 便携载荷检查

构建后从仓库根运行：

```powershell
powershell.exe -NoProfile -File tests/native-runtime-smoke/check-pri.ps1
powershell.exe -NoProfile -File tests/native-runtime-smoke/check-payload-cleanup.ps1
powershell.exe -NoProfile -File tests/native-runtime-smoke/check-sandbox.ps1 -ToolHostPath 'apps/tool-host/bin/Debug/net10.0-windows10.0.19041.0/win-x64/KYNXA.ToolHost.exe'
powershell.exe -NoProfile -File tests/native-runtime-smoke/run.ps1 -BundleRoot 'apps/desktop/bin/x64/Debug/net10.0-windows10.0.26100.0/win-x64' -ResultPath artifacts/development/native-package-check.json
```

载荷复制到带空格的独立临时目录；仅保留系统目录及随包 DLL 在 PATH，隐藏系统 .NET，隔离所有模型、聊天和扩展数据。验证 DLL/模型/许可证 SHA-256，启动随包原生 HTTP/SSE 网关、资源服务和自包含 ToolHost，检查健康、目录与真实工具目录；核心不含 Node、NAPI 或 JS 网关，固定私有 Node 只允许位于 `runtime/mcp`。退出时只关闭本夹具的进程树。

`check-mcp-runtime.ps1 -BundleRoot <本轮载荷目录> -ResultPath <结果路径>` 单独核验私有 Node 官方 SHA、五个固定服务入口及对应浏览器，在 PATH 只有系统目录、环境变量隔离的条件下完成真实 MCP initialize/tools/list；Playwright 访问 about:blank，DBHub 使用内存演示数据库，Brave 使用模拟 Key 且不执行联网搜索。它验证安装入口和真实协议，不代表真实账号、外部数据库或所有第三方扩展已配置。用户机器不调用 npm/npx 或 Playwright install。

`check-pri.ps1` 验证独立运行时保留为普通主包载荷，并保持实际 UI 资源及重复处理的幂等性。上述检查不连接外部 MCP、不调用真实模型、不控制用户窗口，也不代表正式 MSIX 安装、ARM64 或检索质量已经验收。HTTP/SSE 模型和工具续接在 `apps/agent-core/tests/gateway_http.rs` 使用本机模拟供应商另行验证。

`check-sandbox.ps1` 直接使用自包含 ToolHost，在带空格的独立 Data 根中实际执行 CMD AppContainer：核验精确 `Sandboxes/<GUID>` 工作范围、敏感文件排除、只写快照、进程树退出，以及错误根目录、非法 ID、正式聊天数据和 Node 命令的拒绝。标准输入使用明确 UTF-8 字节并保持打开，不能让 Windows PowerShell 的默认字符页或 EOF 取消信号干扰结果。`check-payload-cleanup.ps1` 仅在独立已验证输出夹具中检查旧载荷删除、正式数据保护和重复执行。
