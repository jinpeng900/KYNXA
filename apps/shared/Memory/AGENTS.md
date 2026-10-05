# 记忆共享契约 / Shared memory contracts 开发边界 / Development boundaries

遵循仓库根 AGENTS.md 与两个 KYNXA 开发 Skill。 / Follow the root AGENTS.md and both KYNXA development skills.

- 负责人 / Owner: E。职责 / Scope: 记忆请求、响应与数据版本合同。 / Memory request, response and data revision contracts.
- 与记忆客户端、管理界面和网关联动审查；不改变范围隔离、稳定身份或冲突语义。 / Review with memory clients, management UI and gateway; preserve scope isolation, stable identity and conflict semantics.
- 修改跨模块契约时与调用方协同；目录移动维护所有 csproj 的 Compile Include/Exclude 引用。 / Coordinate contract changes with callers; update all csproj Compile Include/Exclude references when moving sources.
- 保留命名空间、绑定、序列化字段和资源路径，按影响验证，测试仅使用独立临时数据。 / Preserve namespaces, bindings, serialized fields and resource paths; validate the affected behavior using isolated temporary test data.
