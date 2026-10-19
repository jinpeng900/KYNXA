# 用量统计悬浮卡

KYNXA 的主统计入口位于输入框底栏。可选 Windows 桌面卡片展示同一份“本次运行”Token 消耗和缓存命中率，默认由主应用关闭，通过设置开启。卡片沿用宿主的主题与语言，不再使用人物、气泡或消耗阈值。

## 使用

- 拖动卡片顶部移动；位置保存为物理屏幕坐标，恢复时约束到可见工作区。
- 点击右上角按钮折叠或展开；按钮支持 Tab、Enter 和空格。
- 右键菜单提供“恢复右下角”“置顶”和“关闭”。
- 数字仅在真实快照变化时逐位滚动，约 350 ms；系统关闭动画时直接更新。
- 卡片晚开启仍显示主窗口当前累计；关闭或重新开启卡片不重置统计。

## 唯一统计所有者

WinUI 主应用使用 `Core/UsageAccumulator.cs` 和 `Core/UsageLogTail.cs` 读取原生网关的 JSONL 用量投影。正式运行的 WPF 卡片不读取用量日志，不保存累计，不访问模型凭据或网络，不发起模型请求。

仅 `event: call_completed`、`schemaVersion: 1` 的真实模型调用终态进入累计。稳定 `callId` 去重集合最多保存 100,000 项，达到上限后保留已有数据并报告统计不完整。错误或取消调用已有的用量仍计入；缺失字段保持未知，真实 0 保留。输入、输出分别有独立已知计数，只有两者都已知时才能推导总量。缓存率为所有已知命中 token 总和除以对应输入 token 总和；字段缺失、冲突、采集丢失或损坏会保留不完整状态。该投影不替代供应商账单。

增量读取器在主应用启动时跳过既有完整历史，等待新终态行；支持 UTF-8 分片、半行、截断和文件替换。读取失败只报告固定异常类型，不输出模型内容或凭据。

## 宿主管道合同

随包程序为 `UsageMonitor/KYNXA.UsageMonitor.exe`，生产参数：

```text
--data-dir <本机布局目录> --owner-pid <WinUI进程号> --control-stdin
```

同一设置目录只允许一个实例。卡片在 `ContentRendered` 之后从 stdout 返回 `KYNXA_USAGE_MONITOR_READY_V1`；启动或互斥失败返回 `KYNXA_USAGE_MONITOR_ERROR_V1` 并退出。主应用通过继承的私有 stdin 发送 UTF-8 单行 JSON：

```json
{"event":"snapshot","snapshot":{"totalText":"3,714","cacheText":"0.0%","scopeLabel":"本次运行"}}
```

其余字段见 `Core/UsageDisplayState.cs`。命令最多 64 KiB，颜色仅接受 `#RRGGBB`；损坏命令忽略并最多报告一次固定错误类型，不能把传入数据写入诊断。`KYNXA_USAGE_MONITOR_CLOSE_V1`、stdin EOF 或准确宿主进程退出都会关闭卡片。卡片立即持有宿主进程句柄，避免 PID 复用；不按进程名查找或终止其他实例。

仅显式开发模式可自行回放日志：

```powershell
dotnet run --project apps/usage-monitor/KYNXA.UsageMonitor.csproj -- --replay --usage-log '<测试JSONL绝对路径>' --data-dir '<独立测试目录>' --preview-taskbar
```

此模式的窗口标题和范围标签始终包含“演示 / 回放”，不代表本次真实消耗。`--preview-taskbar` 仅用于开发时选择真实窗口。

## 验证

```powershell
dotnet run --project tests/usage-monitor-smoke/UsageMonitorSmoke.csproj
dotnet build apps/usage-monitor/KYNXA.UsageMonitor.csproj
```

Core 冒烟不调用模型，覆盖独立输入/输出覆盖、加权缓存、未知值、失败已有用量、去重边界、日志轮换以及有界私有管道。实际拖动、折叠和宿主启停需要在集成后的真实 Windows 窗口另行检查。

数字滚动保留 [MeteorNOX MIT 来源说明](THIRD_PARTY_NOTICES.md)；不包含上游美术、音效或余额查询代码。
