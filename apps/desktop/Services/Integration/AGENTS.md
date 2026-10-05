# 桌面集成 / Desktop integration 开发边界 / Development boundaries

遵循仓库根 AGENTS.md 与两个 KYNXA 开发 Skill。 / Follow the root AGENTS.md and both KYNXA development skills.

- 负责人 / Owner: A。职责 / Scope: 网关进程启动、就绪握手、统一 HTTP 响应与错误。 / Gateway startup, readiness handshake, HTTP responses and errors.
- 保留地址、就绪时序、取消与响应释放；业务配置分别由模型、工具、数据模块维护。 / Preserve addresses, readiness ordering, cancellation and response ownership; domain configuration belongs to Models, Tools and Data.
- 修改跨模块契约时与调用方协同；目录移动维护所有 csproj 的 Compile Include/Exclude 引用。 / Coordinate contract changes with callers; update all csproj Compile Include/Exclude references when moving sources.
- 保留命名空间、绑定、序列化字段和资源路径，按影响验证，测试仅使用独立临时数据。 / Preserve namespaces, bindings, serialized fields and resource paths; validate the affected behavior using isolated temporary test data.
