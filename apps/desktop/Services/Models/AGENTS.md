# 模型与聊天客户端 / Model and chat clients 开发边界 / Development boundaries

遵循仓库根 AGENTS.md 与两个 KYNXA 开发 Skill。 / Follow the root AGENTS.md and both KYNXA development skills.

- 负责人 / Owner: C。职责 / Scope: 模型连接 API、SSE 读取、目录与预设、模型选择偏好，以及兼容测试用 MockChatClient。 / Model connection API, SSE parsing, catalog, presets, selection preferences and the legacy test MockChatClient.
- 通过 Integration 连接网关，复用共享 Chat 与 Tools 契约；模型选择只保存 UI 偏好。 / Reach the gateway through Integration and use shared Chat and Tools contracts; model selection stores UI preferences only.
- 修改跨模块契约时与调用方协同；目录移动维护所有 csproj 的 Compile Include/Exclude 引用。 / Coordinate contract changes with callers; update all csproj Compile Include/Exclude references when moving sources.
- 保留命名空间、绑定、序列化字段和资源路径，按影响验证，测试仅使用独立临时数据。 / Preserve namespaces, bindings, serialized fields and resource paths; validate the affected behavior using isolated temporary test data.
