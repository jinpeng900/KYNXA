# Desktop boundary / 桌面边界

- Keep the `KYNXA.ToolHost` namespace, request/result JSON and window/PID validation compatible. Keep native declarations in `../Native`; reuse shared placement rather than duplicating policy.
  保持命名空间、请求/结果 JSON 与窗口/PID 校验兼容；原生声明位于 `../Native`，复用位置策略。
- Preserve foreground/UIPI checks, bounded UIA workers and release of only this operation's delivered presses after cancellation or failure.
  保留前台/UIPI 检查、限时 UIA 工作线程及取消或失败后的本次已送达按键释放。
- Use pure injected-input smoke for source layout changes. Real desktop checks may use only fixture-owned windows/processes; never inspect existing user windows.
  源码布局变更使用注入输入的纯逻辑 smoke；真实桌面检查仅使用夹具自有窗口/进程，不读取用户已有窗口。
