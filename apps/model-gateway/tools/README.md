# tools

主责：D · 工具与扩展。

维护工具注册/发现、审批策略、MCP/Skill、浏览器、文件、宿主终端及原生执行适配。

可调用 Data 仓储/工作区/结果归档，复用 Models 纯协议辅助和 Platform；不引用 Orchestration。审批、权限、范围和回执不得因官方来源绕过。

参见 [五人职责](../../../docs/team/README.md)、[依赖与合同](../../../docs/architecture/team-boundaries.md)。根目录 `server.mjs`、`initialize-storage.mjs`、`migrate-storage.mjs` 为稳定启动入口；不要在本目录再创建另一份正式服务或数据源。

跨域变更同步生产调用方、共享 DTO、测试和随包清单。新增/移动模块后运行 `node tools/development/check-architecture.mjs`；回归选择见 [开发规则](../../../.agents/skills/kynxa-development/SKILL.md)。
