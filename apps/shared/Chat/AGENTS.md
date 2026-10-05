# 聊天共享契约 / Shared chat contracts 开发边界 / Development boundaries

遵循仓库根 AGENTS.md 与两个 KYNXA 开发 Skill。 / Follow the root AGENTS.md and both KYNXA development skills.

- 负责人 / Owner: C。职责 / Scope: 聊天请求、回复及流事件的数据合同。 / Data contracts for chat requests, replies and streaming events.
- 与桌面、网关和测试联动审查；不引用 WinUI，不实现 HTTP 或持久化。 / Review with desktop, gateway and tests; do not depend on WinUI or implement HTTP or persistence.
- 修改跨模块契约时与调用方协同；目录移动维护所有 csproj 的 Compile Include/Exclude 引用。 / Coordinate contract changes with callers; update all csproj Compile Include/Exclude references when moving sources.
- 保留命名空间、绑定、序列化字段和资源路径，按影响验证，测试仅使用独立临时数据。 / Preserve namespaces, bindings, serialized fields and resource paths; validate the affected behavior using isolated temporary test data.
