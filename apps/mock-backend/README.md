# 旧版聊天模拟接口（仅回归测试）

该服务固定返回“你好”，不调用模型。`tests/chat-smoke` 仍用它检查旧版聊天契约和存储行为；当前 WinUI 和 Linux 预览已改用 `apps/model-gateway`，不再连接此服务。

在仓库根目录运行旧版回归测试时：

```powershell
dotnet run --project apps/mock-backend/KYNXA.MockBackend.csproj
dotnet run --project tests/chat-smoke/KYNXA.ChatSmoke.csproj
```

服务默认监听 `http://127.0.0.1:5217`。真实模型配置、测试和聊天的启动方式见 [model-gateway](../model-gateway/README.md)。
