# 工具共享契约 / Shared tool contracts 开发边界 / Development boundaries

遵循仓库根 AGENTS.md 与两个 KYNXA 开发 Skill。 / Follow the root AGENTS.md and both KYNXA development skills.

- 负责人 / Owner: D。职责 / Scope: 工具、审批、执行结果及相关协议数据。 / Tool, approval, execution result and related protocol data.
- 与工具客户端、聊天流和网关联动审查；不加入执行副作用或界面依赖。 / Review with tool clients, chat streaming and gateway; keep execution side effects and UI dependencies outside the contracts.
- 修改跨模块契约时与调用方协同；目录移动维护所有 csproj 的 Compile Include/Exclude 引用。 / Coordinate contract changes with callers; update all csproj Compile Include/Exclude references when moving sources.
- 保留命名空间、绑定、序列化字段和资源路径，按影响验证，测试仅使用独立临时数据。 / Preserve namespaces, bindings, serialized fields and resource paths; validate the affected behavior using isolated temporary test data.
