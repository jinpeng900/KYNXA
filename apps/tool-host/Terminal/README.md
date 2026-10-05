# Terminal / 宿主终端通道

Owns explicit host CMD/PowerShell requests, captured output decoding, visible console workers and their deadlines, named pipes and Job cleanup. It runs outside AppContainer and never substitutes for sandbox execution.
负责显式宿主 CMD/PowerShell 请求、捕获输出解码、可见控制台工作进程及其期限、命名管道与 Job 清理。运行于 AppContainer 外，不替代沙箱执行。

The visible runner finds the companion at `AppContext.BaseDirectory/KYNXA.ToolHost.exe` and reuses `DesktopForegroundPlacement`. Keep command/worker/window identities distinct and preserve unknown outcomes after interruption. Interop declarations live in `../Native`; namespace remains `KYNXA.ToolHost`.
可见运行器从程序集运行目录寻找辅助程序并复用桌面位置策略；命令、工作进程及窗口身份分别保留，中断后维持未知结果语义。原生声明位于 `../Native`，命名空间保持不变。

Pure verification: `dotnet run --project tests/native-host-terminal-smoke/HostTerminalDecoderSmoke.csproj` from the repository root. It tests decoding without launching host commands; see [verification](../../../tests/native-host-terminal-smoke/README.md) for process fixtures.
上述纯逻辑命令从仓库根运行，只检查解码而不启动宿主命令；进程夹具详见验证说明。
