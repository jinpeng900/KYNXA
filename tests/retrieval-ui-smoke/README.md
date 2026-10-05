# Native retrieval settings validation / 原生检索设置验证

```powershell
dotnet build tests/retrieval-ui-smoke/RetrievalUiSmoke.csproj -p:Platform=x64
```

Run the generated `RetrievalUiSmoke.exe`. The actual WinUI settings window is shown without activation, backed by an injected fake retrieval API. Checks cover automatic saves, revision conflicts, global inheritance/project overrides, mounted-folder opt-in, source ownership, background jobs, resizing, immediate language changes and closing during pending writes. / 启动生成的 `RetrievalUiSmoke.exe`。真实 WinUI 设置窗口以不抢焦点的方式显示，使用注入的虚构检索 API；检查自动保存、版本冲突、全局继承与工作覆盖、挂载文件夹显式启用、资料归属、后台任务、缩放、即时语言切换及待保存时关闭保护。

The latest result location is written to `%TEMP%/kynxa-retrieval-ui-smoke-latest.txt`. Native screenshots and JSON results stay in the owned temporary directory. These are automation-peer/control checks, not a claim of physical mouse/keyboard or native picker testing. / 最新结果位置写入 `%TEMP%/kynxa-retrieval-ui-smoke-latest.txt`，原生截图与 JSON 结果保留在独立临时目录；这些是自动化接口与真实控件检查，不宣称已验收物理鼠标、键盘或系统文件选择器。
