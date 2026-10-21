# 对话用量客户端检查

链接正式 `UsageApiClient`、共享 DTO 和响应读取器，以注入 HTTP handler 验证对话范围、未知计量、缓存分母、缺字段和切换取消；不启动网关，不读取用户数据，不调用模型。

```powershell
dotnet run --project tests/usage-summary-smoke/UsageSummarySmoke.csproj
```

该检查不代表真实 WinUI 布局、展开和主题切换验收；原生界面由桌面构建及实际界面检查覆盖。
