# 工具客户端与配置 / Tool clients and configuration 开发边界 / Development boundaries

遵循仓库根 AGENTS.md 与两个 KYNXA 开发 Skill。 / Follow the root AGENTS.md and both KYNXA development skills.

- 负责人 / Owner: D。职责 / Scope: 工具 API、浏览器连接设置和 MCP 配置输入转换。 / Tool API, browser connection settings and MCP configuration conversion.
- 桌面发送请求和展示结果，网关和 ToolHost 负责已有执行链；此目录不拥有正式数据迁移。 / The desktop sends requests and presents results; the gateway and ToolHost own existing execution, and Data owns storage migration.
- 修改跨模块契约时与调用方协同；目录移动维护所有 csproj 的 Compile Include/Exclude 引用。 / Coordinate contract changes with callers; update all csproj Compile Include/Exclude references when moving sources.
- 保留命名空间、绑定、序列化字段和资源路径，按影响验证，测试仅使用独立临时数据。 / Preserve namespaces, bindings, serialized fields and resource paths; validate the affected behavior using isolated temporary test data.
