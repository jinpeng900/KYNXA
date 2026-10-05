# Terminal boundary / 宿主终端边界

- Preserve fixed shell selection, public JSON/events, output encoding/limits and distinct captured/visible execution modes. Approvals and immutable scope belong to the gateway.
  保留固定 shell 选择、公开 JSON/事件、输出编码/限制与捕获/可见执行模式；审批和不可变范围归网关。
- Keep process-tree Job ownership, pipe client PID verification, deadlines and handle disposal with the runner. Window host PID can differ from command PID; never terminate a shared terminal host by that PID.
  运行器拥有进程树 Job、管道客户端 PID 校验、期限与句柄释放；窗口宿主 PID 可不同于命令 PID，不按该 PID 终止共享终端宿主。
- Source layout checks use the pure decoder project. Host process/window tests require independently owned temporary fixtures and must not execute user scripts.
  源码布局检查使用纯解码工程；宿主进程/窗口检查须使用独立临时夹具，不执行用户脚本。
