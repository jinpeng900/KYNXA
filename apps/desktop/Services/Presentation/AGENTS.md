# 界面展示服务 / Presentation services 开发边界 / Development boundaries

遵循仓库根 AGENTS.md 与两个 KYNXA 开发 Skill。 / Follow the root AGENTS.md and both KYNXA development skills.

- 负责人 / Owner: B。职责 / Scope: 本地化、侧栏与工作面板状态、聊天和公式展示、工具审批与结果展示。 / Localization, sidebar and work pane state, transcript and formula presentation, tool approval and result presentation.
- 配合 Views、Controls、ViewModels、Models/UI、Layout、Styles 和 Resources；正式聊天与记忆仍由网关保存。 / Work with Views, Controls, ViewModels, Models/UI, Layout, Styles and Resources; the gateway owns authoritative chat and memory storage.
- 修改跨模块契约时与调用方协同；目录移动维护所有 csproj 的 Compile Include/Exclude 引用。 / Coordinate contract changes with callers; update all csproj Compile Include/Exclude references when moving sources.
- 保留命名空间、绑定、序列化字段和资源路径，按影响验证，测试仅使用独立临时数据。 / Preserve namespaces, bindings, serialized fields and resource paths; validate the affected behavior using isolated temporary test data.
