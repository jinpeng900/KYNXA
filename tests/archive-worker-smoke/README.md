# 压缩包 ToolHost 短测

构建当前 Windows x64 ToolHost 后，在 PowerShell 7 运行：

```powershell
./tests/archive-worker-smoke/Run.ps1 -ToolHostExe ./apps/tool-host/bin/Debug/net10.0-windows10.0.19041.0/win-x64/KYNXA.ToolHost.exe
```

自有 TAR.XZ 夹具含一个 `guide.md`，正文为 `中文压缩文本 English archive evidence`（43 UTF-8 字节），SHA-256 为 `fa63f7be4865672d618a1b14678a2054a8b4b6c3bd4e03db6ec07df17abdfa44`。它检查实际 `--document-archive` 输入、流式 TAR 成员、完整回执与退出，覆盖解码流回绕导致的真实失败；不执行归档成员，不接触日常 Data。

该测试只覆盖 TAR.XZ 包装和进程协议。Rust 归档边界、正式 HTTP/E5 接线与其他编解码器分别有其测试，不能用此结果代替全部格式验收。
