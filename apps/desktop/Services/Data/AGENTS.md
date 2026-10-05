# 数据客户端与迁移 / Data clients and migration 开发边界 / Development boundaries

遵循仓库根 AGENTS.md 与两个 KYNXA 开发 Skill。 / Follow the root AGENTS.md and both KYNXA development skills.

- 负责人 / Owner: E。职责 / Scope: 记忆 API、会话目录客户端、统一数据与扩展路径、存储及扩展配置迁移。 / Memory API, conversation catalog client, unified data and extension paths, storage and extension configuration migration.
- ProjectStore 是网关 HTTP 客户端；保持稳定 ID、范围、版本冲突、原文件保护和配置路径解析。 / ProjectStore is a gateway HTTP client; preserve stable IDs, scopes, revision conflicts, source protection and configured path resolution.
- 修改跨模块契约时与调用方协同；目录移动维护所有 csproj 的 Compile Include/Exclude 引用。 / Coordinate contract changes with callers; update all csproj Compile Include/Exclude references when moving sources.
- 保留命名空间、绑定、序列化字段和资源路径，按影响验证，测试仅使用独立临时数据。 / Preserve namespaces, bindings, serialized fields and resource paths; validate the affected behavior using isolated temporary test data.
