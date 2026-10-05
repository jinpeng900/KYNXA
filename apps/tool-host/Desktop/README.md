# Desktop / 桌面通道

Owns selected-window requests, discovery, UI Automation reads, screenshots, application launch, placement and input. The gateway supplies approval/scope; this folder validates window/PID identity and host-desktop constraints.
负责已选窗口请求、发现、UI Automation 读取、截图、程序启动、位置与输入。网关提供审批及范围；本目录验证窗口/PID 身份与宿主桌面约束。

`DesktopInputSequence` tracks only delivered input and releases owned presses. `DesktopRunner` owns bounded worker execution; `DesktopForegroundPlacement` is reused by visible host terminals. Interop declarations live in `../Native`; types retain `KYNXA.ToolHost`.
`DesktopInputSequence` 仅跟踪已送达输入并释放自身按键；`DesktopRunner` 管理限时工作执行，`DesktopForegroundPlacement` 也供可见宿主终端复用。原生声明位于 `../Native`，类型保持 `KYNXA.ToolHost` 命名空间。

Pure verification: `dotnet run --project tests/native-desktop-smoke/InputSequenceSmoke.csproj` from the repository root. Real desktop smoke requires its own fixture windows and serial execution; see [verification](../../../tests/native-desktop-smoke/README.md).
纯逻辑验证从仓库根运行上述命令；真实桌面验证只使用夹具自有窗口并串行执行，详见验证说明。
